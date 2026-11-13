//! Progress window shown for every transfer (sending and receiving). It has
//! the dialog role, so screen readers read all its lines whenever it comes
//! to the front; later changes of a line are not announced (see
//! `PhaseNotice` and `Finished` for what is).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Controls::{
            PBM_SETMARQUEE, PBM_SETPOS, PBM_SETRANGE32, PBM_SETSTATE, PBS_MARQUEE, PBST_ERROR,
            PBST_NORMAL,
        },
        Input::KeyboardAndMouse::GetFocus,
        WindowsAndMessaging::*,
    },
};

use super::{
    ask, confirm, format_duration, format_size, send, set_text, shell, tray,
    window::{self, CreateOptions, Handler, Window},
};
pub use crate::view::{Direction, Speed};
use crate::{
    app,
    config::{CloseAction, ConnectionLostAction, ReceivedFolder},
    i18n::tr,
    net::{Command, Kind, Outcome, Phase},
    view::{can_pause, keeps_progress, moves_data, percent},
};

const ID_CANCEL: u16 = 11;
const ID_PAUSE: u16 = 12;
const ID_BACKGROUND: u16 = 13;
const ID_CHAT: u16 = 14;
const RANGE: usize = 1000;
/// Posted to itself after the transfer ended, to ask or close outside of event handling.
const WM_APP_FINISHED: u32 = WM_APP + 10;
/// Fires a few seconds after the connection was lost, if it is still lost:
/// short interruptions should not bother the user with a question.
const TIMER_LOST: usize = 1;
const LOST_PROMPT_DELAY_MS: u32 = 3000;
/// Fires a moment after a sent item became the receiver's, if the transfer
/// has not ended by then (see `Progress::show_handed_over`).
const TIMER_HANDED_OVER: usize = 2;
const HANDED_OVER_NOTICE_DELAY_MS: u32 = 3000;
/// Height of the status line at the usual text size: two lines.
const STATUS_HEIGHT: i32 = 36;
/// Command links of the questions.
const CHOICE_BACKGROUND: i32 = 100;
const CHOICE_CANCEL: i32 = 101;
const CHOICE_OPEN_FILE: i32 = 102;
const CHOICE_OPEN_FOLDER: i32 = 103;
const CHOICE_WAIT: i32 = 104;
/// How often the speed and the remaining time change, so they can be read.
const RATE_INTERVAL: Duration = Duration::from_secs(1);
/// Shows the speed and the remaining time again, also when no data comes
/// (see `Progress::show_rate`).
const TIMER_RATE: usize = 3;
const MIN_WIDTH: i32 = 460;
const HEIGHT: i32 = 238;

struct Progress {
    id: u64,
    dir: Direction,
    peer: String,
    item: String,
    kind: Kind,
    /// For a received folder: what is kept of it.
    received_folder: Option<ReceivedFolder>,
    phase: Option<Phase>,
    active: bool,
    /// The window was hidden and the transfer continues without it.
    background: bool,
    /// The user closed the window and cancelled the transfer. The window is
    /// only hidden until the transfer has ended: then it closes without a
    /// word if the transfer was cancelled, and reports how it ended if the
    /// cancel came too late (the item was being saved already).
    closing: bool,
    /// The user cancelled the transfer, which has not ended yet: it may wait
    /// for the other side (see `Session::defer_cancel`) or come too late.
    /// The window then neither offers a pause nor asks about a lost
    /// connection, and its status says that the transfer is being cancelled.
    cancel_requested: bool,
    /// Sending side: the cancel waits for the receiver's verdict, and the
    /// transfer goes on until that comes (`Event::CancelWaits`). What holds
    /// the transfer up meanwhile is then told, as it holds up the cancel.
    cancel_waits: bool,
    /// The last phase change withheld its notification about what holds the
    /// transfer up on the other side, because the cancel seemed to end the
    /// transfer in a moment (see `set_phase`); it is given after all if the
    /// cancel turns out to wait (`set_cancel_waits`).
    balloon_withheld: bool,
    /// The last phase other than `Verifying` held the transfer up (a pause
    /// on the other side, a lost connection, another program that keeps the
    /// partly received file out): the next one in which the transfer moves
    /// on gives the notification that it continues. Checking the data after
    /// a restart comes in between, but holds nothing up.
    held_up: bool,
    /// The notification that another program keeps the partly received
    /// file out was given (see `Phase::PartInUse`), and the transfer has not
    /// moved on or been held up otherwise since: it is not given again when
    /// the status comes back to it after a pause.
    part_in_use_told: bool,
    /// Data has flowed while this window was open. Only then is a lost
    /// connection something to ask about; a transfer picked up after a
    /// restart simply waits for the other side.
    had_data: bool,
    /// Sending side: all of the item has arrived and it is the receiver's
    /// (`Event::HandedOver`). Cancelling is no longer possible, so neither
    /// the Cancel button nor closing the window offers it, and a lost
    /// connection asks nothing: the receiver finishes without it.
    handed_over: bool,
    meter: Speed,
    /// When the speed was last worked out (see `RATE_INTERVAL`).
    rate_shown: Option<Instant>,
    /// What the speed and remaining time rows show.
    rate_text: Rate,
    /// The amount done and the total, as last reported.
    progress: (u64, u64),
    /// How the connection goes, as the network last said: directly, through
    /// a relay server, or not at all (see `Event::Path`).
    route: Option<bool>,
    /// For a completed receive: where the item was saved.
    result_path: Option<PathBuf>,
    /// What to do once the transfer has ended.
    after: AfterFinish,
    title: HWND,
    status: HWND,
    bar: HWND,
    done_caption: HWND,
    done: HWND,
    speed: HWND,
    remaining: HWND,
    connection: HWND,
    pause: HWND,
    background_button: HWND,
    cancel: HWND,
}

/// What to tell the user about a finished transfer.
pub struct Finished {
    pub title: String,
    pub text: String,
    /// Revealed when the notification is clicked.
    pub path: Option<PathBuf>,
    /// Report the result with a notification as well. Either the window closes
    /// by itself, or only its text changes, which screen readers do not
    /// announce; the notification is what makes the outcome perceivable.
    pub notify: bool,
}

/// What the window does after the transfer ended. Decided once, in `finish`,
/// together with whether to notify, and carried out on `WM_APP_FINISHED`:
/// deciding again then could disagree with the notification (the window may
/// have been minimized or restored in between).
enum AfterFinish {
    /// Stays open to show what happened.
    Stay,
    Close,
    /// Asks whether to open what arrived at this path, then closes.
    AskOpen(PathBuf),
    /// Tells that the item arrived (the message's main text), then closes.
    TellSent(String),
}

/// What to tell the user about a change of phase.
pub struct PhaseNotice {
    /// A notification (title and text): text changes in the window are not
    /// announced by screen readers, and a hidden window is not seen at all.
    pub balloon: Option<(String, String)>,
}

/// Opens the window. `activate` gives it the focus; transfers that start by
/// themselves (automatic or restored) must not take it away from the user.
pub fn open(
    id: u64,
    dir: Direction,
    peer: &str,
    item: &str,
    kind: Kind,
    size: Option<u64>,
    activate: bool,
) -> HWND {
    let window_title = match (dir, kind) {
        (Direction::Send, Kind::File) => tr!(title_send_file, name = peer),
        (Direction::Send, Kind::Folder) => tr!(title_send_folder, name = peer),
        (Direction::Receive, Kind::File) => tr!(title_receive_file, name = peer),
        (Direction::Receive, Kind::Folder) => tr!(title_receive_folder, name = peer),
    };
    let handler = Progress::new(id, dir, peer, item, kind);
    let hwnd = window::create(
        CreateOptions {
            title: &window_title,
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            client: (MIN_WIDTH, HEIGHT),
            center_on: app::main_window(),
            ..Default::default()
        },
        Box::new(handler),
    );
    if let Some(size) = size {
        window::with_handler::<Progress, _>(hwnd, |p, _| p.set_progress(0, size));
    }
    unsafe {
        if activate {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        } else {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    hwnd
}

/// Tells a receive's window what is kept of a received folder.
pub fn set_received_folder(hwnd: HWND, folder: ReceivedFolder) {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.received_folder = Some(folder));
}

/// Brings a transfer's window back, also from the background or after it
/// was closed while being cancelled: the user asked for it, so it takes part
/// in the finishing questions again and stays to show how the transfer ended.
pub fn show(hwnd: HWND) {
    window::with_handler::<Progress, _>(hwnd, |p, _| {
        p.background = false;
        p.closing = false;
    });
    window::bring_to_front(hwnd);
}

/// Shows the new phase. Returns `None` if the window is busy.
pub fn set_phase(hwnd: HWND, phase: Phase) -> Option<PhaseNotice> {
    window::with_handler::<Progress, _>(hwnd, |p, w| p.set_phase(w, phase))
}

/// Sending side: the item is the receiver's now (see `Event::HandedOver`).
/// Returns `false` if the window is busy.
pub fn set_handed_over(hwnd: HWND) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, w| p.show_handed_over(w)).is_some()
}

/// A send restored with a cancel from the last run (see
/// `TransferRecord::cancel_pending`): the window shows it as being
/// cancelled, as after the user's own cancel. `waits` tells whether the
/// cancel waits on for the receiver's verdict (`TransferRecord::done_sent`),
/// as `Event::CancelWaits` would, rather than ending the transfer as soon as
/// it is issued again.
pub fn set_cancelling(hwnd: HWND, waits: bool) {
    window::with_handler::<Progress, _>(hwnd, |p, _| {
        if waits {
            // No phase has arrived yet, so there is no withheld notification
            // to give.
            p.set_cancel_waits();
        } else {
            p.show_cancelling();
        }
    });
}

/// Sending side: the cancel waits for the receiver's verdict (see
/// `Event::CancelWaits`). Returns `None` if the window is busy.
pub fn set_cancel_waits(hwnd: HWND) -> Option<PhaseNotice> {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.set_cancel_waits())
}

pub fn set_progress(hwnd: HWND, done: u64, total: u64) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.set_progress(done, total)).is_some()
}

/// How the connection goes (see `Event::Path`); `None` when there is none.
pub fn set_path(hwnd: HWND, direct: Option<bool>) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, _| {
        p.route = direct;
        p.show_connection();
    })
    .is_some()
}

/// Shows the final result. `total` is the size of the transfer as its last
/// report had it (0 if none came), which the window may not know: it drops
/// the reports that find it busy. Returns `None` if the window is busy.
pub fn finish(hwnd: HWND, outcome: &Outcome, total: u64) -> Option<Finished> {
    window::with_handler::<Progress, _>(hwnd, |p, w| p.finish(w, outcome, total))
}

/// What the speed and remaining time rows show (see `Progress::rate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rate {
    /// As they are: worked out less than `RATE_INTERVAL` ago.
    Unchanged,
    /// Nothing: no data moves now, or all of it is there.
    Nothing,
    /// Not known yet.
    Calculating,
    /// Bytes per second, and seconds left.
    Known { rate: u64, left: u64 },
    /// The data of a transfer has stopped coming (see `Speed::stalled`).
    Stalled,
}

impl Progress {
    /// The state of a new window, whose controls are made when it is
    /// created.
    fn new(id: u64, dir: Direction, peer: &str, item: &str, kind: Kind) -> Self {
        Progress {
            id,
            dir,
            peer: peer.to_string(),
            item: item.to_string(),
            kind,
            received_folder: None,
            phase: None,
            active: true,
            background: false,
            closing: false,
            cancel_requested: false,
            cancel_waits: false,
            balloon_withheld: false,
            held_up: false,
            part_in_use_told: false,
            had_data: false,
            handed_over: false,
            meter: Speed::default(),
            rate_shown: None,
            rate_text: Rate::Nothing,
            progress: (0, 0),
            route: None,
            result_path: None,
            after: AfterFinish::Stay,
            title: std::ptr::null_mut(),
            status: std::ptr::null_mut(),
            bar: std::ptr::null_mut(),
            done_caption: std::ptr::null_mut(),
            done: std::ptr::null_mut(),
            speed: std::ptr::null_mut(),
            remaining: std::ptr::null_mut(),
            connection: std::ptr::null_mut(),
            pause: std::ptr::null_mut(),
            background_button: std::ptr::null_mut(),
            cancel: std::ptr::null_mut(),
        }
    }

    /// Whether what was received is a single file: a file, or a folder that
    /// was kept only as its zip.
    fn received_file(&self) -> bool {
        self.kind == Kind::File || self.received_folder == Some(ReceivedFolder::KeepZip)
    }

    fn marquee(&self, on: bool) {
        unsafe {
            let style = GetWindowLongW(self.bar, GWL_STYLE) as u32;
            let new = if on {
                style | PBS_MARQUEE
            } else {
                style & !PBS_MARQUEE
            };
            if new != style {
                SetWindowLongW(self.bar, GWL_STYLE, new as i32);
            }
        }
        send(self.bar, PBM_SETMARQUEE, on as usize, 30);
    }

    fn clear_rate(&mut self) {
        for h in [self.speed, self.remaining] {
            set_text(h, "");
        }
        self.meter.reset();
        self.rate_shown = None;
        self.rate_text = Rate::Nothing;
    }

    /// The speed and the remaining time at `now` (see `Speed`): only while
    /// data moves and some is still to come, and in a transfer a speed of
    /// zero once its data has stopped coming. The other phases are this
    /// computer's own work, whose reports can pause without anything wrong
    /// (a folder with many empty files).
    fn rate(&self, now: Instant) -> Rate {
        let (done, total) = self.progress;
        if !moves_data(self.phase) || done >= total {
            return Rate::Nothing;
        }
        if self.phase == Some(Phase::Transferring) && self.meter.stalled(now) {
            return Rate::Stalled;
        }
        if self
            .rate_shown
            .is_some_and(|t| now.duration_since(t) < RATE_INTERVAL)
        {
            return Rate::Unchanged;
        }
        match self.meter.rate() {
            Some(rate) if rate > 0 => Rate::Known {
                rate,
                left: (total - done) / rate,
            },
            _ if self.rate_shown.is_none() => Rate::Calculating,
            _ => Rate::Unchanged,
        }
    }

    /// Shows the speed and the remaining time (see `rate`), on each report
    /// and every second, so that they also show when data stops coming.
    fn show_rate(&mut self, now: Instant) {
        let rate = self.rate(now);
        if let Rate::Known { .. } = rate {
            self.rate_shown = Some(now);
        }
        if rate == self.rate_text {
            return;
        }
        let (speed, remaining) = match rate {
            Rate::Unchanged => return,
            Rate::Nothing => (String::new(), String::new()),
            Rate::Calculating => (tr!(calculating).to_string(), tr!(calculating).to_string()),
            Rate::Known { rate, left } => (
                tr!(speed_value, rate = format_size(rate)),
                tr!(remaining_value, left = format_duration(left)),
            ),
            Rate::Stalled => {
                // Data that comes again is measured anew.
                self.rate_shown = None;
                // No report comes that would update the notification area
                // icon's tooltip.
                app::transfer_stalled(self.id);
                (tr!(speed_value, rate = format_size(0)), String::new())
            }
        };
        self.rate_text = rate;
        set_text(self.speed, &speed);
        set_text(self.remaining, &remaining);
    }

    /// The connection line: how the data goes, as the network last said
    /// (`route`); nothing while there is no connection.
    fn connection_text(&self) -> &'static str {
        match self.route {
            Some(true) => tr!(connection_direct),
            Some(false) => tr!(connection_relay),
            None => "",
        }
    }

    /// Shows `connection_text`, until the transfer ends: then the line says
    /// where the item went or what failed.
    fn show_connection(&self) {
        if self.active {
            set_text(self.connection, self.connection_text());
        }
    }

    /// The status line in `phase`.
    fn status_text(&self, phase: Phase) -> String {
        match phase {
            // A cancel can wait for the other side (a send whose data it may
            // have all of), which still says how the transfer is doing.
            _ if self.cancel_requested && !self.handed_over => tr!(cancelling).to_string(),
            // Sending side, waiting for the receiver to save the item: this
            // also says why Cancel is unavailable. A pause still says so
            // itself: the sender does not reconnect while paused, so the
            // result only comes once the user resumes.
            Phase::Transferring | Phase::WaitingForConnection { .. } if self.handed_over => {
                tr!(phase_handed_over, name = self.peer)
            }
            Phase::Preparing => tr!(phase_preparing).to_string(),
            Phase::Connecting => tr!(phase_connecting, name = self.peer),
            Phase::WaitingForAccept => tr!(phase_waiting, name = self.peer),
            Phase::Transferring => match self.dir {
                Direction::Send => tr!(phase_sending).to_string(),
                Direction::Receive => tr!(phase_receiving).to_string(),
            },
            Phase::Paused => tr!(phase_paused).to_string(),
            Phase::PausedByPeer => tr!(phase_paused_by_peer, name = self.peer),
            Phase::WaitingForConnection { .. } => tr!(phase_waiting_connection).to_string(),
            Phase::PartInUse { .. } => match self.dir {
                Direction::Send => tr!(phase_peer_part_in_use, name = self.peer),
                Direction::Receive => tr!(phase_part_in_use).to_string(),
            },
            Phase::Verifying => tr!(phase_verifying).to_string(),
            Phase::Unpacking => tr!(phase_unpacking).to_string(),
            Phase::PeerUnpacking => tr!(phase_peer_unpacking, name = self.peer),
        }
    }

    fn set_phase(&mut self, w: &Window, phase: Phase) -> PhaseNotice {
        let previous = self.phase.replace(phase);
        if phase == Phase::Transferring {
            self.had_data = true;
        }
        if phase == Phase::PeerUnpacking {
            // The notification says why Cancel goes away.
            self.hand_over();
        }
        set_text(self.status, &self.status_text(phase));
        set_text(
            self.done_caption,
            match phase {
                Phase::Preparing | Phase::Unpacking | Phase::Verifying => tr!(row_processed),
                _ => tr!(row_transferred),
            },
        );
        // Waiting for the other side, with nothing to count here.
        let waiting = matches!(
            phase,
            Phase::Connecting | Phase::WaitingForAccept | Phase::PeerUnpacking
        );
        self.marquee(waiting);
        // A pause or a lost connection interrupts the transfer; the amount
        // done so far stays (also what a restored transfer showed before its
        // first phase, and all of it once the receiver is unpacking). Other
        // phases start their own count.
        let continues = keeps_progress(phase)
            && previous.is_none_or(|p| keeps_progress(p) || p == Phase::PeerUnpacking);
        if !waiting && !continues {
            send(self.bar, PBM_SETPOS, 0, 0);
            set_text(self.done, "");
        }
        self.clear_rate();

        set_text(
            self.pause,
            if phase == Phase::Paused {
                tr!(resume)
            } else {
                tr!(pause)
            },
        );
        window::enable(self.pause, can_pause(phase) && !self.cancel_requested);

        // A connection lost while data was flowing asks (or acts) after a
        // moment (`connection_lost`, which leaves alone a transfer that the
        // user has cancelled already). A transfer restored at startup just
        // waits, which the "unfinished transfer" notification already said,
        // and so does one whose other side exited on purpose (`lost: false`).
        let lost = matches!(phase, Phase::WaitingForConnection { lost: true }) && self.had_data;
        if lost {
            unsafe { SetTimer(w.hwnd, TIMER_LOST, LOST_PROMPT_DELAY_MS, None) };
        }
        let will_ask = lost
            && !self.background
            && !self.handed_over
            && !self.cancel_requested
            && app::cfg().connection_lost == ConnectionLostAction::Ask;
        PhaseNotice {
            balloon: self.phase_balloon(previous, will_ask),
        }
    }

    /// The notification for the phase that has just come after `previous`,
    /// if it gives one; none for a lost connection that the window asks
    /// about (`will_ask`).
    fn phase_balloon(
        &mut self,
        previous: Option<Phase>,
        will_ask: bool,
    ) -> Option<(String, String)> {
        let phase = self.phase?;
        let was_held_up = self.held_up;
        self.held_up = match phase {
            Phase::PausedByPeer | Phase::WaitingForConnection { .. } | Phase::PartInUse { .. } => {
                true
            }
            Phase::Verifying => was_held_up,
            _ => false,
        };
        if !matches!(
            phase,
            Phase::Paused | Phase::Verifying | Phase::PartInUse { .. }
        ) {
            self.part_in_use_told = false;
        }
        // Not again once the receiver's last step has waited for another
        // program there, which is told as a continuation instead.
        let unpacking_there = phase == Phase::PeerUnpacking
            && !matches!(
                previous,
                Some(Phase::PeerUnpacking | Phase::PartInUse { finishing: true })
            );
        // A cancelled transfer ends for the cancel in a moment, whatever the
        // other side does, unless the cancel waits for the receiver's verdict
        // (`cancel_waits`) or came too late, with the item the receiver's
        // (`handed_over`): then the transfer goes on until the receiver has
        // answered, which a lost connection can hold up for long.
        let ends_for_cancel = self.cancel_requested && !self.cancel_waits && !self.handed_over;
        // What holds the transfer up on the other side holds up a cancel
        // that waits for it, or the end of a transfer whose cancel came too
        // late, just the same, so it is told also to a user who has
        // cancelled, perhaps with the window minimized or closed, who would
        // otherwise not learn why the transfer does not end. Withheld from
        // one whose cancel ends the transfer in a moment, and given after
        // all if the cancel turns out to wait (`set_cancel_waits`: whether
        // it does may only be known a moment after the cancel).
        let held_up = if will_ask {
            None
        } else {
            self.held_up_balloon(phase)
        };
        self.balloon_withheld = ends_for_cancel && held_up.is_some();
        let balloon = match phase {
            // The status line changes, and Cancel goes away (perhaps with
            // the focus on it): this says why, also to a user who cancelled
            // (too late) and perhaps closed the window. The unpacking can
            // take a while, unlike the receiver's renaming of a file.
            Phase::PeerUnpacking if unpacking_there => Some((
                if self.cancel_requested {
                    tr!(notify_too_late_title)
                } else {
                    tr!(tx_peer_unpacking)
                }
                .to_string(),
                tr!(peer_unpacking_balloon, item = self.item, name = self.peer),
            )),
            Phase::PausedByPeer | Phase::WaitingForConnection { .. } | Phase::PartInUse { .. }
                if held_up.is_some() && !ends_for_cancel =>
            {
                held_up
            }
            // Being cancelled otherwise: the transfer ends for the cancel in
            // a moment, and the result says so.
            _ if self.closing || self.cancel_requested => None,
            // The status line changes, which a screen reader does not say.
            Phase::Transferring | Phase::Unpacking | Phase::PeerUnpacking if was_held_up => Some((
                tr!(continuing_balloon_title).to_string(),
                tr!(continuing_balloon, item = self.item),
            )),
            _ => None,
        };
        self.note_part_in_use_told(&balloon);
        balloon
    }

    /// What holds the transfer up, if `phase` says so (the peer paused it,
    /// the connection was lost while data flowed, or another program keeps
    /// the partly received file out, here or there, the first time): the
    /// notification that tells it.
    fn held_up_balloon(&self, phase: Phase) -> Option<(String, String)> {
        match phase {
            Phase::PausedByPeer => Some((
                tr!(paused_balloon_title).to_string(),
                tr!(paused_by_peer_balloon, item = self.item, name = self.peer),
            )),
            Phase::WaitingForConnection { lost: true } if self.had_data => Some((
                tr!(connection_lost_balloon_title).to_string(),
                tr!(connection_lost_balloon, item = self.item),
            )),
            Phase::PartInUse { .. } if !self.part_in_use_told => Some((
                tr!(part_in_use_balloon_title).to_string(),
                match self.dir {
                    Direction::Send => {
                        tr!(peer_part_in_use_balloon, item = self.item, name = self.peer)
                    }
                    Direction::Receive => tr!(part_in_use_balloon, item = self.item),
                },
            )),
            _ => None,
        }
    }

    /// Notes that the notification `balloon`, given in the current phase,
    /// told that another program keeps the partly received file out.
    fn note_part_in_use_told(&mut self, balloon: &Option<(String, String)>) {
        if balloon.is_some() && matches!(self.phase, Some(Phase::PartInUse { .. })) {
            self.part_in_use_told = true;
        }
    }

    /// `Event::CancelWaits`: the cancel waits for the receiver's verdict, so
    /// what holds the transfer up holds up the cancel (see `cancel_waits`).
    /// A notification about that which the last phase change withheld, as
    /// the cancel seemed to end the transfer in a moment, is given now: the
    /// phase is still the one it was withheld for.
    fn set_cancel_waits(&mut self) -> PhaseNotice {
        if !self.active {
            return PhaseNotice { balloon: None };
        }
        self.show_cancelling();
        self.cancel_waits = true;
        let balloon = if std::mem::take(&mut self.balloon_withheld) {
            self.phase.and_then(|phase| self.held_up_balloon(phase))
        } else {
            None
        };
        self.note_part_in_use_told(&balloon);
        PhaseNotice { balloon }
    }

    fn set_progress(&mut self, done: u64, total: u64) {
        if !self.active {
            return;
        }
        let pos = if total == 0 {
            RANGE
        } else {
            (done as u128 * RANGE as u128 / total as u128) as usize
        };
        send(self.bar, PBM_SETPOS, pos, 0);
        set_text(
            self.done,
            &tr!(
                progress_of,
                done = format_size(done),
                total = format_size(total),
                percent = percent(done, total)
            ),
        );

        // Speed over the last few seconds.
        let now = Instant::now();
        self.progress = (done, total);
        self.meter.note(now, done);
        self.show_rate(now);
    }

    fn finish(&mut self, w: &Window, outcome: &Outcome, total: u64) -> Finished {
        self.active = false;
        self.marquee(false);
        unsafe {
            KillTimer(w.hwnd, TIMER_LOST);
            KillTimer(w.hwnd, TIMER_HANDED_OVER);
            KillTimer(w.hwnd, TIMER_RATE);
        }
        let file = self.kind == Kind::File;
        let mut completed = false;
        let (status, detail) = match (outcome, self.dir) {
            (Outcome::Completed(_), Direction::Send) => {
                completed = true;
                let text = if file {
                    tr!(done_sent_file, name = self.peer)
                } else {
                    tr!(done_sent_folder, name = self.peer)
                };
                (text, String::new())
            }
            (Outcome::Completed(path), Direction::Receive) => {
                completed = true;
                self.result_path = path.clone();
                let location = path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                (self.saved_text().to_string(), location)
            }
            (Outcome::Declined, Direction::Send) => {
                (tr!(declined_by_peer, name = self.peer), String::new())
            }
            (Outcome::Declined, Direction::Receive) => (tr!(declined).into(), String::new()),
            (Outcome::Cancelled, _) => (tr!(cancelled).into(), String::new()),
            (Outcome::CancelledByPeer, _) => {
                (tr!(cancelled_by_peer, name = self.peer), String::new())
            }
            (Outcome::Unreachable(e), _) => (tr!(unreachable, name = self.peer), e.clone()),
            (Outcome::Failed(e), _) => (tr!(failed).into(), e.clone()),
        };
        set_text(self.status, &status);
        set_text(self.speed, "");
        set_text(self.remaining, "");
        // The connection no longer matters; show where the item went or what failed.
        set_text(self.connection, &detail);
        if completed {
            send(self.bar, PBM_SETSTATE, PBST_NORMAL as usize, 0);
            send(self.bar, PBM_SETPOS, RANGE, 0);
            // The last reports may have found the window busy (a question
            // was open), or all of them from the first one on, and the text
            // would stay at what it was: the size is the caller's then.
            let total = if total > 0 { total } else { self.progress.1 };
            if total > 0 {
                set_text(
                    self.done,
                    &tr!(
                        progress_of,
                        done = format_size(total),
                        total = format_size(total),
                        percent = percent(total, total)
                    ),
                );
            }
        } else {
            send(self.bar, PBM_SETSTATE, PBST_ERROR as usize, 0);
        }
        // Only Close is left. If the focus was on a button that goes away,
        // it moves to Close (the window is active then, so this takes nothing
        // away from another window).
        let focus = unsafe { GetFocus() };
        if focus == self.pause || focus == self.background_button {
            window::focus(self.cancel);
        }
        window::show(self.pause, false);
        window::show(self.background_button, false);
        set_text(self.cancel, tr!(close));
        window::enable(self.cancel, true);

        let received = completed && self.dir == Direction::Receive;
        let sent = completed && self.dir == Direction::Send;
        let (ask_after_receive, show_after_send, close_when_ended) = app::with(|a| {
            (
                a.cfg.ask_after_receive,
                a.cfg.show_after_send,
                a.cfg.close_when_ended,
            )
        });
        // A completed transfer's window closes by itself, after the question
        // or the message, if any; a failed one stays to show what happened,
        // unless the user wants every window closed at the end: then none
        // asks or stays, and the notification says how the transfer ended.
        // A hidden or minimized window asks nothing: a question needs a
        // window that is seen, and the notification reports instead. Nor does
        // one while others with the same person are under way: several items
        // of a selection would pile up a dialog each. The last to finish asks.
        let hidden = self.background || window::is_minimized(w.hwnd);
        self.after = if close_when_ended {
            AfterFinish::Close
        } else if !completed && !self.background {
            AfterFinish::Stay
        } else if hidden || app::others_under_way(self.id) {
            AfterFinish::Close
        } else if received
            && ask_after_receive
            && let Some(path) = &self.result_path
        {
            AfterFinish::AskOpen(path.clone())
        } else if sent && show_after_send {
            AfterFinish::TellSent(status.clone())
        } else {
            AfterFinish::Close
        };
        if matches!(self.after, AfterFinish::Stay) {
            unsafe { FlashWindow(w.hwnd, 1) };
        } else {
            unsafe { PostMessageW(w.hwnd, WM_APP_FINISHED, 0, 0) };
        }

        // Completed although the user cancelled: the cancel came too late.
        let title = if completed && self.cancel_requested {
            tr!(notify_too_late_title)
        } else if completed {
            tr!(notify_done_title)
        } else {
            tr!(notify_failed_title)
        }
        .to_string();
        let text = if received {
            tr!(notify_saved, item = self.item, name = self.peer)
        } else {
            tr!(notify_status, item = self.item, status = status)
        };
        // The question asked after a receive and the message after a send
        // are announced by themselves, and a cancel that the user closed the
        // window for needs no word; every other outcome is reported with a
        // notification too.
        let announced = matches!(
            self.after,
            AfterFinish::AskOpen(_) | AfterFinish::TellSent(_)
        );
        let expected = self.closing && matches!(outcome, Outcome::Cancelled);
        Finished {
            title,
            text,
            path: self.result_path.clone(),
            notify: !announced && !expected,
        }
    }

    /// Sending side: all of the item has arrived, and it is the receiver's
    /// now. A cancel would come too late (the network side no longer ends
    /// the transfer for it), so the button is unavailable from now on, and
    /// the focus moves on from it. `None` if nothing changed (handed over
    /// already, or the transfer is over), otherwise whether that is a change
    /// to tell the user about: the user has cancelled, which came too late,
    /// or the focus was on the button, since screen readers announce only
    /// where the focus went. The caller sets the status line, which says why.
    fn hand_over(&mut self) -> Option<bool> {
        if self.handed_over || !self.active {
            return None;
        }
        self.handed_over = true;
        let had_focus = unsafe { GetFocus() } == self.cancel;
        if had_focus {
            window::focus(self.background_button);
        }
        window::enable(self.cancel, false);
        Some(self.cancel_requested || had_focus)
    }

    /// `Event::HandedOver`: the hand-over, with the status line that says
    /// why Cancel went away. A notification says so too, but only a moment
    /// later, if the transfer is still running then: the receiver renames a
    /// file in an instant, and the result says it all (a folder that is
    /// being unpacked there is announced with its phase instead).
    fn show_handed_over(&mut self, w: &Window) {
        let Some(tell) = self.hand_over() else {
            return;
        };
        if let Some(phase) = self.phase {
            set_text(self.status, &self.status_text(phase));
        }
        if tell {
            unsafe { SetTimer(w.hwnd, TIMER_HANDED_OVER, HANDED_OVER_NOTICE_DELAY_MS, None) };
        }
    }

    /// The notification of `show_handed_over`, a moment later.
    fn handed_over_notice(&self) {
        let title = if self.cancel_requested {
            tr!(notify_too_late_title)
        } else {
            tr!(handed_over_title)
        };
        tray::balloon(
            app::main_window(),
            title,
            &tr!(handed_over_balloon, item = self.item),
            Some(tray::BalloonAction::Transfer(self.id)),
        );
    }

    fn cancel_transfer(&mut self) {
        app::net_send(Command::Cancel { id: self.id });
        self.show_cancelling();
    }

    /// The transfer is being cancelled: it can be neither cancelled again nor
    /// paused, and the status line says so (see `cancel_requested`).
    fn show_cancelling(&mut self) {
        if self.cancel_requested || !self.active {
            return;
        }
        self.cancel_requested = true;
        if !self.handed_over {
            set_text(self.status, tr!(cancelling));
        }
        let focus = unsafe { GetFocus() };
        if focus == self.cancel || focus == self.pause {
            window::focus(self.background_button);
        }
        window::enable(self.cancel, false);
        window::enable(self.pause, false);
    }

    /// Hides the window of a transfer that the user cancelled until it has
    /// ended: a cancel that comes too late leaves the item saved, and the
    /// user must learn that (see `closing`).
    fn hide_while_cancelling(&mut self, w: &Window) {
        self.closing = true;
        self.background = true;
        unsafe { KillTimer(w.hwnd, TIMER_LOST) };
        window::show(w.hwnd, false);
    }

    fn toggle_pause(&mut self) {
        if self.phase == Some(Phase::Paused) {
            app::net_send(Command::Resume { id: self.id });
        } else {
            app::net_send(Command::Pause { id: self.id });
        }
    }

    /// Hides the window; the transfer goes on and reports with notifications.
    fn go_background(&mut self, w: &Window) {
        self.background = true;
        window::show(w.hwnd, false);
        tray::balloon(
            app::main_window(),
            tr!(background_balloon_title),
            &tr!(background_balloon, item = self.item),
            Some(tray::BalloonAction::Transfer(self.id)),
        );
    }

    /// Closing the window while the transfer runs: continue in the background
    /// or cancel, as chosen now or remembered from before. Once the item is
    /// the receiver's, there is nothing to choose: the transfer can only
    /// continue. Once the user has cancelled, there is nothing to choose
    /// either: the window is hidden until the transfer has ended, as when
    /// closing it cancels.
    fn close_running(&mut self, w: &Window) {
        if self.cancel_requested && !self.handed_over {
            self.hide_while_cancelling(w);
            return;
        }
        let action = match app::cfg().close_running_transfer {
            _ if self.handed_over => CloseAction::Background,
            CloseAction::Ask => {
                let (choice, dont_ask) = ask(
                    w.hwnd,
                    tr!(close_running_instruction),
                    tr!(close_running_text),
                    &[
                        (CHOICE_BACKGROUND, tr!(continue_background)),
                        (CHOICE_CANCEL, tr!(cancel_transfer)),
                    ],
                    false,
                    Some(tr!(dont_ask_again)),
                );
                let action = match choice {
                    CHOICE_BACKGROUND => CloseAction::Background,
                    CHOICE_CANCEL => CloseAction::Cancel,
                    _ => return,
                };
                if dont_ask {
                    app::update_cfg(|c| c.close_running_transfer = action);
                }
                action
            }
            remembered => remembered,
        };
        if action == CloseAction::Cancel {
            if confirm(w.hwnd, tr!(cancel_confirm)) {
                self.cancel_transfer();
                self.hide_while_cancelling(w);
            }
        } else {
            self.go_background(w);
        }
    }

    /// The connection has been lost for a few seconds: wait or cancel, as
    /// chosen now or in the settings. The network side waits anyway, so
    /// waiting needs nothing more.
    fn connection_lost(&mut self, w: &Window) {
        if !self.active
            || self.closing
            || self.cancel_requested
            || self.handed_over
            || !self.had_data
            || !matches!(self.phase, Some(Phase::WaitingForConnection { .. }))
        {
            return;
        }
        let action = match app::cfg().connection_lost {
            ConnectionLostAction::Ask => {
                if self.background {
                    // A question needs a visible window.
                    self.background = false;
                    window::bring_to_front(w.hwnd);
                }
                let (choice, dont_ask) = ask(
                    w.hwnd,
                    tr!(connection_lost_instruction),
                    tr!(connection_lost_text),
                    &[
                        (CHOICE_WAIT, tr!(wait_for_connection)),
                        (CHOICE_CANCEL, tr!(cancel_transfer)),
                    ],
                    false,
                    Some(tr!(dont_ask_again)),
                );
                let action = match choice {
                    CHOICE_WAIT => ConnectionLostAction::Wait,
                    CHOICE_CANCEL => ConnectionLostAction::Cancel,
                    _ => return,
                };
                if dont_ask {
                    app::update_cfg(|c| c.connection_lost = action);
                }
                action
            }
            remembered => remembered,
        };
        if action == ConnectionLostAction::Cancel && self.active {
            self.cancel_transfer();
        }
    }

    /// The status of a completed receive.
    fn saved_text(&self) -> &'static str {
        match (self.kind, self.received_folder) {
            (Kind::File, _) => tr!(done_saved_file),
            (Kind::Folder, Some(ReceivedFolder::KeepZip)) => tr!(done_saved_folder_zip),
            (Kind::Folder, _) => tr!(done_saved_folder),
        }
    }

    /// After a completed send: tells the user that the item arrived.
    fn tell_sent(&self, w: &Window, instruction: &str) {
        let item = tr!(sent_item, item = self.item);
        let (_, dont_show) = ask(
            w.hwnd,
            instruction,
            &item,
            &[],
            true,
            Some(tr!(dont_show_again)),
        );
        if dont_show {
            app::update_cfg(|c| c.show_after_send = false);
        }
    }

    /// After a completed receive: offers to open what arrived.
    fn ask_open(&self, w: &Window, path: &Path) {
        let file = self.received_file();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.item.clone());
        let folder = path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let choices: &[(i32, &str)] = if file {
            &[
                (CHOICE_OPEN_FILE, tr!(open_file)),
                (CHOICE_OPEN_FOLDER, tr!(open_containing_folder)),
            ]
        } else {
            &[(CHOICE_OPEN_FOLDER, tr!(open_received_folder))]
        };
        let (choice, dont_ask) = ask(
            w.hwnd,
            self.saved_text(),
            &tr!(finished_question, item = name, folder = folder),
            choices,
            true,
            Some(tr!(dont_ask_again)),
        );
        if dont_ask {
            app::update_cfg(|c| c.ask_after_receive = false);
        }
        match choice {
            CHOICE_OPEN_FILE => {
                shell::open_path(path);
            }
            CHOICE_OPEN_FOLDER if file => shell::reveal_in_explorer(path),
            CHOICE_OPEN_FOLDER => {
                shell::open_path(path);
            }
            _ => {}
        }
    }
}

impl Handler for Progress {
    fn on_create(&mut self, w: &Window) {
        w.set_dialog_role();
        // The buttons are as wide as their (translated) texts need; the
        // window grows with them.
        let pause_w = w
            .button_width(tr!(pause), 92)
            .max(w.button_width(tr!(resume), 92));
        let background_w = w.button_width(tr!(minimize_background), 92);
        let cancel_w = w
            .button_width(tr!(cancel), 92)
            .max(w.button_width(tr!(close), 92));
        let chat_w = w.button_width(tr!(chat_button), 0);
        let width = MIN_WIDTH.max(chat_w + pause_w + background_w + cancel_w + 2 * 6 + 16 + 28);
        let cw = width - 28;
        // The status line holds two lines at the usual text size. The
        // longest texts (with the other side's name, which can be long) may
        // need more, and so may a larger Windows text size, which the DPI
        // scaling of the layout does not follow; everything below moves
        // down by the difference.
        let name = self.peer.as_str();
        let longest = [
            tr!(phase_handed_over, name = name),
            tr!(phase_peer_unpacking, name = name),
            tr!(phase_paused_by_peer, name = name),
            tr!(phase_connecting, name = name),
            tr!(phase_waiting, name = name),
            tr!(done_sent_file, name = name),
            tr!(done_sent_folder, name = name),
            tr!(declined_by_peer, name = name),
            tr!(cancelled_by_peer, name = name),
            tr!(unreachable, name = name),
            tr!(phase_waiting_connection).to_string(),
            tr!(phase_paused).to_string(),
        ];
        let status_h = longest
            .iter()
            .map(|text| w.text_height(text, cw))
            .max()
            .unwrap_or(STATUS_HEIGHT)
            .max(STATUS_HEIGHT);
        let dy = status_h - STATUS_HEIGHT;
        if width > MIN_WIDTH || dy > 0 {
            w.set_client_size(width, HEIGHT + dy);
        }

        let title = match self.dir {
            Direction::Send => tr!(to_peer, name = self.peer, item = self.item),
            Direction::Receive => tr!(from_peer, name = self.peer, item = self.item),
        };
        self.title = w.label(&title, (14, 12, cw, 20));
        w.make_bold(self.title);
        self.status = w.label(tr!(starting), (14, 38, cw, status_h));
        self.bar = w.add("msctls_progress32", "", 0, 0, (14, 78 + dy, cw, 16));
        // Otherwise named after the status line, which screen readers would
        // then leave out when they read the window.
        w.set_accessible_name(self.bar, tr!(progress_bar));
        send(self.bar, PBM_SETRANGE32, 0, RANGE as isize);

        // "Caption: value" rows; the captions share the width of the longest one.
        let captions = [
            tr!(row_transferred),
            tr!(row_processed),
            tr!(row_speed),
            tr!(row_remaining),
        ];
        let caption_w = captions.iter().map(|c| w.text_width(c)).max().unwrap_or(0) + 8;
        let value_x = 14 + caption_w;
        let value_w = cw - caption_w;
        let row = |caption: &str, y: i32| {
            let c = w.label(caption, (14, y + dy, caption_w, 18));
            (c, w.label("", (value_x, y + dy, value_w, 18)))
        };
        (self.done_caption, self.done) = row(tr!(row_transferred), 104);
        self.speed = row(tr!(row_speed), 124).1;
        self.remaining = row(tr!(row_remaining), 144).1;

        self.connection = w.label("", (14, 168 + dy, cw, 34));
        // Right-aligned buttons: Pause, Minimize to background, Cancel.
        let cancel_x = width - 14 - cancel_w;
        let background_x = cancel_x - 6 - background_w;
        let pause_x = background_x - 6 - pause_w;
        let button_y = 200 + dy;
        // Chat with the other side stands apart on the left, and comes
        // first in the tab order, as on the screen.
        w.button(tr!(chat_button), ID_CHAT, (14, button_y, chat_w, 28));
        self.pause = w.button(tr!(pause), ID_PAUSE, (pause_x, button_y, pause_w, 28));
        window::enable(self.pause, false);
        self.background_button = w.button(
            tr!(minimize_background),
            ID_BACKGROUND,
            (background_x, button_y, background_w, 28),
        );
        self.cancel = w.button(tr!(cancel), ID_CANCEL, (cancel_x, button_y, cancel_w, 28));
        w.default_id.set(ID_CANCEL);
        // Pause is enabled only once data flows, so the focus starts on the
        // first button that can be used.
        window::focus(self.background_button);
        unsafe { SetTimer(w.hwnd, TIMER_RATE, RATE_INTERVAL.as_millis() as u32, None) };
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        match id {
            ID_CHAT => {
                if let Some((peer, name)) = app::transfer_peer(self.id) {
                    super::chat::open(&peer, &name, true);
                }
            }
            ID_PAUSE if self.active => self.toggle_pause(),
            ID_BACKGROUND if self.active => self.go_background(w),
            ID_CANCEL if self.active => {
                if confirm(w.hwnd, tr!(cancel_confirm)) {
                    self.cancel_transfer();
                }
            }
            // Escape behaves like closing the window.
            2 if self.active => self.close_running(w),
            ID_CANCEL | 2 => w.close(),
            _ => return false,
        }
        true
    }

    fn on_message(&mut self, w: &Window, msg: u32, wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_APP_FINISHED => {
                let after = std::mem::replace(&mut self.after, AfterFinish::Stay);
                if matches!(after, AfterFinish::AskOpen(_) | AfterFinish::TellSent(_))
                    && window::is_minimized(w.hwnd)
                {
                    // Minimized since `finish`: no notification went out,
                    // so the question has to be seen.
                    window::bring_to_front(w.hwnd);
                }
                match after {
                    AfterFinish::Stay => return Some(0),
                    AfterFinish::Close => {}
                    AfterFinish::AskOpen(path) => self.ask_open(w, &path),
                    AfterFinish::TellSent(text) => self.tell_sent(w, &text),
                }
                w.close();
                Some(0)
            }
            WM_TIMER if wp == TIMER_LOST => {
                unsafe { KillTimer(w.hwnd, TIMER_LOST) };
                self.connection_lost(w);
                Some(0)
            }
            WM_TIMER if wp == TIMER_RATE => {
                if self.active {
                    self.show_rate(Instant::now());
                }
                Some(0)
            }
            WM_TIMER if wp == TIMER_HANDED_OVER => {
                unsafe { KillTimer(w.hwnd, TIMER_HANDED_OVER) };
                if self.active {
                    self.handed_over_notice();
                }
                Some(0)
            }
            _ => None,
        }
    }

    fn on_close(&mut self, w: &Window) {
        if self.active {
            self.close_running(w);
        } else {
            w.close();
        }
    }

    fn on_destroy(&mut self, _w: &Window) {
        app::transfer_window_closed(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::STALLED_AFTER;

    /// Goes to `phase` as `set_phase` does; the title of the notification
    /// that it gives, if any.
    fn step(p: &mut Progress, phase: Phase) -> Option<String> {
        let previous = p.phase.replace(phase);
        if phase == Phase::Transferring {
            p.had_data = true;
        }
        p.phase_balloon(previous, false).map(|(title, _)| title)
    }

    const IN_USE: Phase = Phase::PartInUse { finishing: false };
    const IN_USE_FINISHING: Phase = Phase::PartInUse { finishing: true };

    /// The window shows a speed while data moves and some is still to come,
    /// and zero once the data of a transfer has stopped coming; the
    /// connection line says nothing while there is no connection.
    #[test]
    fn the_speed_and_the_connection_follow_the_data() {
        let mut p = Progress::new(1, Direction::Receive, "Ana", "a.bin", Kind::File);
        let start = Instant::now();
        let later = start + Duration::from_secs(1);
        p.phase = Some(Phase::Transferring);
        p.progress = (0, 1000);
        p.meter.note(start, 0);
        assert_eq!(p.rate(start), Rate::Calculating);
        assert_eq!(
            p.rate(start + STALLED_AFTER),
            Rate::Calculating,
            "nothing came yet"
        );
        p.meter.note(later, 100);
        p.progress = (100, 1000);
        assert_eq!(p.rate(later), Rate::Known { rate: 100, left: 9 });
        assert_eq!(p.rate(later + STALLED_AFTER), Rate::Stalled);
        p.phase = Some(Phase::Unpacking);
        assert_ne!(
            p.rate(later + STALLED_AFTER),
            Rate::Stalled,
            "this side's own work"
        );
        p.phase = Some(Phase::WaitingForConnection { lost: true });
        assert_eq!(p.rate(later), Rate::Nothing);
        p.phase = None;
        assert_eq!(p.rate(later), Rate::Nothing, "restored, before its phase");
        p.phase = Some(Phase::Transferring);
        p.progress = (1000, 1000);
        assert_eq!(p.rate(later), Rate::Nothing, "all of it is there");

        // A rate worked out a moment ago stays, also when it came out the
        // same as the one shown.
        p.progress = (100, 1000);
        p.show_rate(later);
        let again = later + RATE_INTERVAL;
        p.show_rate(again);
        assert_eq!(p.rate_shown, Some(again));
        assert_eq!(p.rate(again + Duration::from_millis(100)), Rate::Unchanged);

        p.route = Some(false);
        assert_eq!(p.connection_text(), tr!(connection_relay));
        p.route = None;
        assert_eq!(p.connection_text(), "");
    }

    /// Another program that keeps the partly received file out is told
    /// once, also when the status comes back to it after a pause; that the
    /// transfer goes on is told also after the data is checked again, as
    /// after a restart; and a new hold-up is told again.
    #[test]
    fn a_part_in_use_is_told_once_and_so_is_its_end() {
        let waiting = Some(tr!(part_in_use_balloon_title).to_string());
        let continues = Some(tr!(continuing_balloon_title).to_string());
        let mut p = Progress::new(1, Direction::Receive, "Ana", "a.txt", Kind::File);
        assert_eq!(
            step(&mut p, Phase::WaitingForConnection { lost: false }),
            None
        );
        assert_eq!(step(&mut p, IN_USE), waiting);
        assert_eq!(step(&mut p, Phase::Paused), None);
        assert_eq!(step(&mut p, IN_USE), None, "not again after a pause");
        assert_eq!(step(&mut p, Phase::Verifying), None);
        assert_eq!(step(&mut p, Phase::Transferring), continues);
        assert_eq!(step(&mut p, IN_USE), waiting, "held up again");
        assert_eq!(
            step(&mut p, Phase::WaitingForConnection { lost: true }),
            Some(tr!(connection_lost_balloon_title).to_string())
        );
        assert_eq!(step(&mut p, IN_USE), waiting, "after something else");
        // The user resumed after the program let go: nothing to tell.
        assert_eq!(step(&mut p, Phase::Paused), None);
        assert_eq!(step(&mut p, Phase::Transferring), None);

        // The last step waits, and then unpacks; Pause is not offered then.
        let mut p = Progress::new(1, Direction::Receive, "Ana", "Photos", Kind::Folder);
        assert_eq!(step(&mut p, Phase::Transferring), None);
        assert_eq!(step(&mut p, Phase::Unpacking), None);
        assert_eq!(step(&mut p, IN_USE_FINISHING), waiting);
        assert_eq!(step(&mut p, Phase::Unpacking), continues);
        assert!(can_pause(IN_USE) && !can_pause(IN_USE_FINISHING));

        // A receive that is being cancelled ends in a moment: nothing told.
        let mut p = Progress::new(1, Direction::Receive, "Ana", "a.txt", Kind::File);
        p.cancel_requested = true;
        assert_eq!(step(&mut p, IN_USE_FINISHING), None);
    }

    /// The sending side is told when a program on the receiving side keeps
    /// the part out, with the receiver's name, also while it unpacks there;
    /// the unpacking is not told again once that program lets go.
    #[test]
    fn the_sender_is_told_about_a_part_in_use_there() {
        let mut p = Progress::new(1, Direction::Send, "Ana", "Photos", Kind::Folder);
        p.handed_over = true;
        assert_eq!(step(&mut p, Phase::Transferring), None);
        assert_eq!(
            step(&mut p, Phase::PeerUnpacking),
            Some(tr!(tx_peer_unpacking).to_string())
        );
        let previous = p.phase.replace(IN_USE_FINISHING);
        let (title, text) = p.phase_balloon(previous, false).unwrap();
        assert_eq!(title, tr!(part_in_use_balloon_title));
        assert!(text.contains("Ana") && text.contains("Photos"), "{text}");
        assert!(p.status_text(IN_USE_FINISHING).contains("Ana"));
        assert_eq!(
            step(&mut p, Phase::PeerUnpacking),
            Some(tr!(continuing_balloon_title).to_string())
        );

        // Also to a user whose cancel waits for the receiver's verdict,
        // which the program there holds up.
        let mut p = Progress::new(1, Direction::Send, "Ana", "a.txt", Kind::File);
        p.cancel_requested = true;
        p.cancel_waits = true;
        assert_eq!(
            step(&mut p, IN_USE_FINISHING),
            Some(tr!(part_in_use_balloon_title).to_string())
        );
    }
}
