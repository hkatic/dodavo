//! Progress window shown for every transfer (sending and receiving). It has
//! the dialog role, so screen readers read all its lines whenever it comes
//! to the front; later changes of a line are not announced (see
//! `PhaseNotice` and `Finished` for what is).

use std::{
    collections::VecDeque,
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
use crate::{
    app,
    config::{CloseAction, ConnectionLostAction, ReceivedFolder},
    i18n::tr,
    net::{Command, Kind, Outcome, Phase},
};

const ID_CANCEL: u16 = 11;
const ID_PAUSE: u16 = 12;
const ID_BACKGROUND: u16 = 13;
const RANGE: usize = 1000;
/// Posted to itself after the transfer ended, to ask or close outside of event handling.
const WM_APP_FINISHED: u32 = WM_APP + 10;
/// Fires a few seconds after the connection was lost, if it is still lost:
/// short interruptions should not bother the user with a question.
const TIMER_LOST: usize = 1;
const LOST_PROMPT_DELAY_MS: u32 = 3000;
/// Command links of the questions.
const CHOICE_BACKGROUND: i32 = 100;
const CHOICE_CANCEL: i32 = 101;
const CHOICE_OPEN_FILE: i32 = 102;
const CHOICE_OPEN_FOLDER: i32 = 103;
const CHOICE_WAIT: i32 = 104;
/// How often the speed and the remaining time change, so they can be read.
const RATE_INTERVAL: Duration = Duration::from_secs(1);
const MIN_WIDTH: i32 = 460;
const HEIGHT: i32 = 238;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

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
    /// Data has flowed while this window was open. Only then is a lost
    /// connection something to ask about; a transfer picked up after a
    /// restart simply waits for the other side.
    had_data: bool,
    samples: VecDeque<(Instant, u64)>,
    rate_shown: Option<Instant>,
    /// For a completed receive: where the item was saved.
    result_path: Option<PathBuf>,
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
    let handler = Progress {
        id,
        dir,
        peer: peer.to_string(),
        item: item.to_string(),
        kind,
        received_folder: None,
        phase: None,
        active: true,
        background: false,
        had_data: false,
        samples: VecDeque::new(),
        rate_shown: None,
        result_path: None,
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
    };
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

/// Brings a transfer's window back, also from the background: the user asked
/// for it, so it takes part in the finishing questions again.
pub fn show(hwnd: HWND) {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.background = false);
    window::bring_to_front(hwnd);
}

/// Shows the new phase. Returns `None` if the window is busy.
pub fn set_phase(hwnd: HWND, phase: Phase) -> Option<PhaseNotice> {
    window::with_handler::<Progress, _>(hwnd, |p, w| p.set_phase(w, phase))
}

pub fn set_progress(hwnd: HWND, done: u64, total: u64) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.set_progress(done, total)).is_some()
}

pub fn set_path(hwnd: HWND, direct: bool) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, _| {
        set_text(
            p.connection,
            if direct {
                tr!(connection_direct)
            } else {
                tr!(connection_relay)
            },
        );
    })
    .is_some()
}

/// Shows the final result. Returns `None` if the window is busy.
pub fn finish(hwnd: HWND, outcome: &Outcome) -> Option<Finished> {
    window::with_handler::<Progress, _>(hwnd, |p, w| p.finish(w, outcome))
}

fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        100
    } else {
        (done as u128 * 100 / total as u128).min(100) as u64
    }
}

/// Phases between which the progress so far stays on screen.
fn keeps_progress(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Transferring
            | Phase::Paused
            | Phase::PausedByPeer
            | Phase::WaitingForConnection { .. }
    )
}

/// Phases in which the user can pause or resume.
fn can_pause(phase: Phase) -> bool {
    keeps_progress(phase)
}

impl Progress {
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
        self.samples.clear();
        self.rate_shown = None;
    }

    fn set_phase(&mut self, w: &Window, phase: Phase) -> PhaseNotice {
        let previous = self.phase.replace(phase);
        if phase == Phase::Transferring {
            self.had_data = true;
        }
        let text = match phase {
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
            Phase::Verifying => tr!(phase_verifying).to_string(),
            Phase::Unpacking => tr!(phase_unpacking).to_string(),
        };
        set_text(self.status, &text);
        set_text(
            self.done_caption,
            match phase {
                Phase::Preparing | Phase::Unpacking | Phase::Verifying => tr!(row_processed),
                _ => tr!(row_transferred),
            },
        );
        let waiting = matches!(phase, Phase::Connecting | Phase::WaitingForAccept);
        self.marquee(waiting);
        // A pause or a lost connection interrupts the transfer; the amount
        // done so far stays (also what a restored transfer showed before its
        // first phase). Other phases start their own count.
        let continues = keeps_progress(phase) && previous.is_none_or(keeps_progress);
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
        window::enable(self.pause, can_pause(phase));

        // A connection lost while data was flowing asks (or acts) after a
        // moment. A transfer restored at startup just waits, which the
        // "unfinished transfer" notification already said, and so does one
        // whose other side exited on purpose (`lost: false`).
        let lost = matches!(phase, Phase::WaitingForConnection { lost: true }) && self.had_data;
        if lost {
            unsafe { SetTimer(w.hwnd, TIMER_LOST, LOST_PROMPT_DELAY_MS, None) };
        }
        let will_ask =
            lost && !self.background && app::cfg().connection_lost == ConnectionLostAction::Ask;
        let balloon = match phase {
            Phase::PausedByPeer => Some((
                tr!(paused_balloon_title).to_string(),
                tr!(paused_by_peer_balloon, item = self.item, name = self.peer),
            )),
            Phase::WaitingForConnection { .. } if lost && !will_ask => Some((
                tr!(connection_lost_balloon_title).to_string(),
                tr!(connection_lost_balloon, item = self.item),
            )),
            Phase::Transferring
                if matches!(
                    previous,
                    Some(Phase::PausedByPeer | Phase::WaitingForConnection { .. })
                ) =>
            {
                Some((
                    tr!(continuing_balloon_title).to_string(),
                    tr!(continuing_balloon, item = self.item),
                ))
            }
            _ => None,
        };
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
        self.samples.push_back((now, done));
        while self.samples.len() > 2
            && now.duration_since(self.samples[0].0) > Duration::from_secs(4)
        {
            self.samples.pop_front();
        }
        if self
            .rate_shown
            .is_some_and(|t| now.duration_since(t) < RATE_INTERVAL)
        {
            return;
        }
        let (t0, d0) = self.samples[0];
        let secs = now.duration_since(t0).as_secs_f64();
        if secs > 0.5 && done > d0 {
            let rate = (done - d0) as f64 / secs;
            let left = (total.saturating_sub(done) as f64 / rate) as u64;
            set_text(
                self.speed,
                &tr!(speed_value, rate = format_size(rate as u64)),
            );
            set_text(
                self.remaining,
                &tr!(remaining_value, left = format_duration(left)),
            );
            self.rate_shown = Some(now);
        } else if self.rate_shown.is_none() && done < total {
            set_text(self.speed, tr!(calculating));
            set_text(self.remaining, tr!(calculating));
        }
    }

    fn finish(&mut self, w: &Window, outcome: &Outcome) -> Finished {
        self.active = false;
        self.marquee(false);
        unsafe { KillTimer(w.hwnd, TIMER_LOST) };
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
        // A hidden or minimized window asks nothing (see `WM_APP_FINISHED`).
        let hidden = self.background || unsafe { IsIconic(w.hwnd) } != 0;
        let will_ask = !hidden && received && app::cfg().ask_after_receive;
        let will_tell = !hidden && sent && app::cfg().show_after_send;
        // A completed transfer's window closes by itself, after the question
        // or the message, if any; a failed one stays to show what happened.
        if self.background || received || sent {
            unsafe { PostMessageW(w.hwnd, WM_APP_FINISHED, 0, 0) };
        } else {
            unsafe { FlashWindow(w.hwnd, 1) };
        }

        let title = if completed {
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
        // are announced by themselves; every other outcome is reported with
        // a notification too.
        Finished {
            title,
            text,
            path: self.result_path.clone(),
            notify: !(will_ask || will_tell),
        }
    }

    fn cancel_transfer(&mut self) {
        app::net_send(Command::Cancel { id: self.id });
        set_text(self.status, tr!(cancelling));
        window::enable(self.cancel, false);
        window::enable(self.pause, false);
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
            Some(tray::BalloonAction::Window(w.hwnd)),
        );
    }

    /// Closing the window while the transfer runs: continue in the background
    /// or cancel, as chosen now or remembered from before.
    fn close_running(&mut self, w: &Window) {
        let action = match app::cfg().close_running_transfer {
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
                w.close();
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

    /// After a completed send: tell the user unless they chose not to be
    /// told, then close.
    fn after_sent(&mut self, w: &Window) {
        if app::cfg().show_after_send {
            let instruction = if self.kind == Kind::File {
                tr!(done_sent_file, name = self.peer)
            } else {
                tr!(done_sent_folder, name = self.peer)
            };
            let item = tr!(sent_item, item = self.item);
            let (_, dont_show) = ask(
                w.hwnd,
                &instruction,
                &item,
                &[],
                true,
                Some(tr!(dont_show_again)),
            );
            if dont_show {
                app::update_cfg(|c| c.show_after_send = false);
            }
        }
        w.close();
    }

    /// After a completed receive: offer to open what arrived, then close.
    fn after_received(&mut self, w: &Window) {
        if let Some(path) = self.result_path.clone()
            && app::cfg().ask_after_receive
        {
            self.ask_open(w, &path);
        }
        w.close();
    }

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
        let width = MIN_WIDTH.max(pause_w + background_w + cancel_w + 2 * 6 + 28);
        if width > MIN_WIDTH {
            w.set_client_size(width, HEIGHT);
        }
        let cw = width - 28;

        let title = match self.dir {
            Direction::Send => tr!(to_peer, name = self.peer, item = self.item),
            Direction::Receive => tr!(from_peer, name = self.peer, item = self.item),
        };
        self.title = w.label(&title, (14, 12, cw, 20));
        w.make_bold(self.title);
        self.status = w.label(tr!(starting), (14, 38, cw, 36));
        self.bar = w.add("msctls_progress32", "", 0, 0, (14, 78, cw, 16));
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
            let c = w.label(caption, (14, y, caption_w, 18));
            (c, w.label("", (value_x, y, value_w, 18)))
        };
        (self.done_caption, self.done) = row(tr!(row_transferred), 104);
        self.speed = row(tr!(row_speed), 124).1;
        self.remaining = row(tr!(row_remaining), 144).1;

        self.connection = w.label("", (14, 168, cw, 34));
        // Right-aligned buttons: Pause, Minimize to background, Cancel.
        let cancel_x = width - 14 - cancel_w;
        let background_x = cancel_x - 6 - background_w;
        let pause_x = background_x - 6 - pause_w;
        self.pause = w.button(tr!(pause), ID_PAUSE, (pause_x, 200, pause_w, 28));
        window::enable(self.pause, false);
        self.background_button = w.button(
            tr!(minimize_background),
            ID_BACKGROUND,
            (background_x, 200, background_w, 28),
        );
        self.cancel = w.button(tr!(cancel), ID_CANCEL, (cancel_x, 200, cancel_w, 28));
        w.default_id.set(ID_CANCEL);
        // Pause is enabled only once data flows, so the focus starts on the
        // first button that can be used.
        window::focus(self.background_button);
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        match id {
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
                // A question needs a visible window; otherwise the
                // notification has reported the outcome.
                let minimized = unsafe { IsIconic(w.hwnd) } != 0;
                if self.background || minimized {
                    w.close();
                } else if self.dir == Direction::Receive {
                    self.after_received(w);
                } else {
                    self.after_sent(w);
                }
                Some(0)
            }
            WM_TIMER if wp == TIMER_LOST => {
                unsafe { KillTimer(w.hwnd, TIMER_LOST) };
                self.connection_lost(w);
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
