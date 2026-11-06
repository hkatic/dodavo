//! What a user interface shows of a transfer, worked out from what the network
//! says: status lines, speed, what can be done with it now, and what to tell
//! the user. Nothing here knows of windows. The Mac interface shows
//! [`TransferView`] as it is; the Win32 one takes the stateless parts so far and
//! keeps its own state in `ui::progress`, which follows the same rules.

use std::{
    collections::VecDeque,
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{
    config::ReceivedFolder,
    format::{format_duration, format_size},
    net::{Kind, Outcome, Phase},
};

/// How long the data of a transfer may stop coming before its speed is zero
/// (see [`Speed::stalled`]): a computer that is switched off or loses its
/// network is noticed only after half a minute.
pub const STALLED_AFTER: Duration = Duration::from_secs(5);
/// The reports of the last this long give the speed; reports further apart
/// do not make one together.
const SPEED_WINDOW: Duration = Duration::from_secs(4);
/// How often the speed and the remaining time change, so they can be read.
const RATE_INTERVAL: Duration = Duration::from_secs(1);
/// How long after the hand-over its notification comes, if the transfer is
/// still running then: the receiver renames a file in an instant, and the
/// result says it all.
const HANDED_OVER_NOTICE_DELAY: Duration = Duration::from_secs(3);

use crate::i18n::{os_tr, tr};

/// The speed of a transfer from its progress reports.
#[derive(Default)]
pub struct Speed {
    /// The reports (time, bytes done) of the last few seconds: at least two,
    /// once there are, so that they span some time.
    samples: VecDeque<(Instant, u64)>,
    /// When the amount done last grew in this phase.
    moved: Option<Instant>,
}

impl Speed {
    /// Starts over: a new phase may count something else, and after a pause
    /// or a lost connection the old speed says nothing.
    pub fn reset(&mut self) {
        *self = Speed::default();
    }

    /// Notes the report that `done` bytes are done, at `now`.
    pub fn note(&mut self, now: Instant, done: u64) {
        if let Some(&(at, before)) = self.samples.back() {
            if done > before {
                self.moved = Some(now);
            }
            // Data that comes again after a while starts a new measurement.
            if now.duration_since(at) > SPEED_WINDOW {
                self.samples.clear();
            }
        }
        self.samples.push_back((now, done));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > SPEED_WINDOW {
            self.samples.pop_front();
        }
    }

    /// Bytes per second over the recent reports, once they span half a
    /// second in which the amount grew.
    pub fn rate(&self) -> Option<u64> {
        let (&(t0, d0), &(t1, d1)) = (self.samples.front()?, self.samples.back()?);
        let secs = t1.duration_since(t0).as_secs_f64();
        (secs > 0.5 && d1 > d0).then(|| ((d1 - d0) as f64 / secs) as u64)
    }

    /// The amount grew in this phase, but not for [`STALLED_AFTER`] up to
    /// `now`. Not before it grew: a sender that checks what it sent before a
    /// restart sends nothing for a while on a connection that is fine.
    pub fn stalled(&self, now: Instant) -> bool {
        self.moved
            .is_some_and(|at| now.duration_since(at) >= STALLED_AFTER)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

pub fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        100
    } else {
        (done as u128 * 100 / total as u128).min(100) as u64
    }
}

/// Phases between which the progress so far stays on screen.
pub fn keeps_progress(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Transferring
            | Phase::Paused
            | Phase::PausedByPeer
            | Phase::WaitingForConnection { .. }
            | Phase::PartInUse { .. }
    )
}

/// Phases in which the user can pause or resume.
pub fn can_pause(phase: Phase) -> bool {
    keeps_progress(phase) && phase != Phase::PartInUse { finishing: true }
}

/// Phases in which data moves on this side, so that it has a speed.
pub fn moves_data(phase: Option<Phase>) -> bool {
    matches!(
        phase,
        Some(Phase::Preparing | Phase::Transferring | Phase::Verifying | Phase::Unpacking)
    )
}

/// One line of state for the list of recipients and the menus, such as
/// “Sending – “report.pdf” (45%)”.
pub fn describe(phase: Option<Phase>, dir: Direction, done: u64, total: u64, item: &str) -> String {
    let state = match phase {
        None => tr!(starting),
        Some(Phase::Preparing) => tr!(tx_preparing),
        Some(Phase::Connecting) => tr!(tx_connecting),
        Some(Phase::WaitingForAccept) => tr!(tx_waiting_accept),
        Some(Phase::Transferring) => match dir {
            Direction::Send => tr!(tx_sending),
            Direction::Receive => tr!(tx_receiving),
        },
        Some(Phase::Paused) => tr!(tx_paused),
        Some(Phase::PausedByPeer) => tr!(tx_paused_by_peer),
        Some(Phase::WaitingForConnection { .. }) => tr!(tx_waiting_connection),
        Some(Phase::PartInUse { .. }) => match dir {
            Direction::Send => os_tr!(tx_peer_part_in_use),
            Direction::Receive => os_tr!(tx_part_in_use),
        },
        Some(Phase::Verifying) => tr!(tx_verifying),
        Some(Phase::Unpacking) => tr!(tx_unpacking),
        Some(Phase::PeerUnpacking) => tr!(tx_peer_unpacking),
    };
    let with_percent = total > 0
        && matches!(
            phase,
            Some(
                Phase::Transferring
                    | Phase::Paused
                    | Phase::PausedByPeer
                    | Phase::WaitingForConnection { .. }
                    | Phase::PartInUse { .. }
            )
        );
    if with_percent {
        let percent = (done as u128 * 100 / total as u128).min(100);
        tr!(
            transfer_status_percent,
            state = state,
            item = item,
            percent = percent
        )
    } else {
        tr!(transfer_status, state = state, item = item)
    }
}

/// What to tell the user about a transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub title: String,
    pub text: String,
}

/// A transfer as a user interface shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub id: u64,
    pub peer_id: String,
    pub peer: String,
    pub sending: bool,
    pub item: String,
    /// Still running. Once it is not: `succeeded` or not.
    pub active: bool,
    pub succeeded: bool,
    /// What it is doing, or how it ended.
    pub status: String,
    /// One line, for the list of recipients and the menus.
    pub state: String,
    /// How much is done, from 0 to 1; `None` while there is nothing to count.
    pub fraction: Option<f64>,
    /// "12 MB of 100 MB (12%)".
    pub amount: String,
    pub speed: String,
    pub remaining: String,
    /// How the data goes, or once it ended, where the item went or what failed.
    pub detail: String,
    pub can_pause: bool,
    /// Paused by the user here: Resume instead of Pause.
    pub paused: bool,
    pub can_cancel: bool,
    /// Where a received item was saved.
    pub path: Option<PathBuf>,
    /// Data is being worked on: the computer must not go to sleep by itself.
    pub moving_data: bool,
}

/// What the speed and remaining time rows show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rate {
    /// Nothing: no data moves now, or all of it is there.
    Nothing,
    /// Not known yet.
    Calculating,
    /// Bytes per second, and seconds left.
    Known { rate: u64, left: u64 },
    /// The data of a transfer has stopped coming (see [`Speed::stalled`]).
    Stalled,
}

/// A transfer, from what the network says of it.
pub struct TransferView {
    id: u64,
    dir: Direction,
    peer_id: String,
    peer: String,
    item: String,
    kind: Kind,
    /// For a received folder: what is kept of it.
    received_folder: Option<ReceivedFolder>,
    phase: Option<Phase>,
    active: bool,
    succeeded: bool,
    /// The user cancelled the transfer, which has not ended yet: it may wait
    /// for the other side (see `Session::defer_cancel`) or come too late.
    cancel_requested: bool,
    /// Sending side: the cancel waits for the receiver's verdict, and the
    /// transfer goes on until that comes (`Event::CancelWaits`). What holds the
    /// transfer up meanwhile is then told, as it holds up the cancel.
    cancel_waits: bool,
    /// The last phase change withheld its notification about what holds the
    /// transfer up, because the cancel seemed to end the transfer in a moment;
    /// it is given after all if the cancel turns out to wait.
    notice_withheld: bool,
    /// The last phase other than `Verifying` held the transfer up: the next
    /// one in which the transfer moves on says that it continues.
    held_up: bool,
    /// That another program keeps the partly received file out was told, and
    /// the transfer has not moved on or been held up otherwise since.
    part_in_use_told: bool,
    /// Data has flowed. Only then is a lost connection something to tell; a
    /// transfer picked up after a restart simply waits for the other side.
    had_data: bool,
    /// Sending side: all of the item has arrived and it is the receiver's.
    handed_over: bool,
    /// When to say so, if the transfer is still running then.
    handed_over_notice_at: Option<Instant>,
    meter: Speed,
    rate_shown: Option<Instant>,
    rate: Rate,
    /// The amount done and the total, as last reported.
    progress: (u64, u64),
    /// How the connection goes: directly, through a relay, or not at all.
    route: Option<bool>,
    status: String,
    /// Once it ended: where the item went or what failed.
    ended_detail: String,
    result_path: Option<PathBuf>,
}

impl TransferView {
    pub fn new(
        id: u64,
        dir: Direction,
        peer_id: &str,
        peer: &str,
        item: &str,
        kind: Kind,
        total: u64,
    ) -> Self {
        TransferView {
            id,
            dir,
            peer_id: peer_id.to_string(),
            peer: peer.to_string(),
            item: item.to_string(),
            kind,
            received_folder: None,
            phase: None,
            active: true,
            succeeded: false,
            cancel_requested: false,
            cancel_waits: false,
            notice_withheld: false,
            held_up: false,
            part_in_use_told: false,
            had_data: false,
            handed_over: false,
            handed_over_notice_at: None,
            meter: Speed::default(),
            rate_shown: None,
            rate: Rate::Nothing,
            progress: (0, total),
            route: None,
            status: tr!(starting).to_string(),
            ended_detail: String::new(),
            result_path: None,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub fn set_received_folder(&mut self, folder: ReceivedFolder) {
        self.received_folder = Some(folder);
    }

    pub fn set_progress(&mut self, done: u64, total: u64) {
        self.progress = (done, total);
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
                Direction::Send => os_tr!(phase_peer_part_in_use, name = self.peer),
                Direction::Receive => os_tr!(phase_part_in_use).to_string(),
            },
            Phase::Verifying => tr!(phase_verifying).to_string(),
            Phase::Unpacking => tr!(phase_unpacking).to_string(),
            Phase::PeerUnpacking => tr!(phase_peer_unpacking, name = self.peer),
        }
    }

    /// A new phase, and what to tell the user about it, if anything.
    pub fn on_phase(&mut self, phase: Phase) -> Option<Notice> {
        let previous = self.phase.replace(phase);
        if phase == Phase::Transferring {
            self.had_data = true;
        }
        if phase == Phase::PeerUnpacking {
            // Its notice says why Cancel goes away.
            self.hand_over();
        }
        self.status = self.status_text(phase);
        // A pause or a lost connection interrupts the transfer; the amount
        // done so far stays (also what a restored transfer showed before its
        // first phase, and all of it once the receiver is unpacking). Other
        // phases start their own count.
        let waiting = Self::waits_without_count(phase);
        let continues = keeps_progress(phase)
            && previous.is_none_or(|p| keeps_progress(p) || p == Phase::PeerUnpacking);
        if !waiting && !continues {
            self.progress.0 = 0;
        }
        self.meter.reset();
        self.rate_shown = None;
        self.rate = Rate::Nothing;
        self.phase_notice(previous)
    }

    fn waits_without_count(phase: Phase) -> bool {
        matches!(
            phase,
            Phase::Connecting | Phase::WaitingForAccept | Phase::PeerUnpacking
        )
    }

    fn phase_notice(&mut self, previous: Option<Phase>) -> Option<Notice> {
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
        // or came too late: then the transfer goes on until the receiver has
        // answered, which a lost connection can hold up for long.
        let ends_for_cancel = self.cancel_requested && !self.cancel_waits && !self.handed_over;
        let held_up = self.held_up_notice(phase);
        self.notice_withheld = ends_for_cancel && held_up.is_some();
        let notice = match phase {
            Phase::PeerUnpacking if unpacking_there => Some(Notice {
                title: if self.cancel_requested {
                    tr!(notify_too_late_title)
                } else {
                    tr!(tx_peer_unpacking)
                }
                .to_string(),
                text: tr!(peer_unpacking_balloon, item = self.item, name = self.peer),
            }),
            Phase::PausedByPeer | Phase::WaitingForConnection { .. } | Phase::PartInUse { .. }
                if held_up.is_some() && !ends_for_cancel =>
            {
                held_up
            }
            // Being cancelled otherwise: the transfer ends for the cancel in
            // a moment, and the result says so.
            _ if self.cancel_requested => None,
            Phase::Transferring | Phase::Unpacking | Phase::PeerUnpacking if was_held_up => {
                Some(Notice {
                    title: tr!(continuing_balloon_title).to_string(),
                    text: tr!(continuing_balloon, item = self.item),
                })
            }
            _ => None,
        };
        self.note_part_in_use_told(&notice);
        notice
    }

    fn held_up_notice(&self, phase: Phase) -> Option<Notice> {
        match phase {
            Phase::PausedByPeer => Some(Notice {
                title: tr!(paused_balloon_title).to_string(),
                text: tr!(paused_by_peer_balloon, item = self.item, name = self.peer),
            }),
            Phase::WaitingForConnection { lost: true } if self.had_data => Some(Notice {
                title: tr!(connection_lost_balloon_title).to_string(),
                text: tr!(connection_lost_balloon, item = self.item),
            }),
            Phase::PartInUse { .. } if !self.part_in_use_told => Some(Notice {
                title: tr!(part_in_use_balloon_title).to_string(),
                text: match self.dir {
                    Direction::Send => {
                        os_tr!(peer_part_in_use_balloon, item = self.item, name = self.peer)
                    }
                    Direction::Receive => os_tr!(part_in_use_balloon, item = self.item),
                },
            }),
            _ => None,
        }
    }

    fn note_part_in_use_told(&mut self, notice: &Option<Notice>) {
        if notice.is_some() && matches!(self.phase, Some(Phase::PartInUse { .. })) {
            self.part_in_use_told = true;
        }
    }

    /// The user cancelled the transfer, which has not ended yet.
    pub fn on_cancel(&mut self) {
        if self.cancel_requested || !self.active {
            return;
        }
        self.cancel_requested = true;
        if !self.handed_over {
            self.status = tr!(cancelling).to_string();
        }
    }

    /// `Event::CancelWaits`: what holds the transfer up holds up the cancel. A
    /// notice about that which the last phase change withheld is given now.
    pub fn on_cancel_waits(&mut self) -> Option<Notice> {
        if !self.active {
            return None;
        }
        self.on_cancel();
        self.cancel_waits = true;
        let notice = if std::mem::take(&mut self.notice_withheld) {
            self.phase.and_then(|phase| self.held_up_notice(phase))
        } else {
            None
        };
        self.note_part_in_use_told(&notice);
        notice
    }

    /// A restored send with a cancel from the last run: shown as being
    /// cancelled. `waits` as with [`on_cancel_waits`](Self::on_cancel_waits).
    pub fn restore_cancel(&mut self, waits: bool) {
        self.on_cancel();
        self.cancel_waits = waits;
    }

    /// Whether the item is the other side's by now; `false` if nothing
    /// changed (handed over already, or the transfer is over).
    fn hand_over(&mut self) -> bool {
        if self.handed_over || !self.active {
            return false;
        }
        self.handed_over = true;
        true
    }

    /// `Event::HandedOver`: the status line says why Cancel went away, and
    /// [`tick`](Self::tick) gives a notice a moment later, if the user had
    /// cancelled (which came too late) or `focus_on_cancel`.
    pub fn on_handed_over(&mut self, now: Instant, focus_on_cancel: bool) {
        if !self.hand_over() {
            return;
        }
        if let Some(phase) = self.phase {
            self.status = self.status_text(phase);
        }
        if self.cancel_requested || focus_on_cancel {
            self.handed_over_notice_at = Some(now + HANDED_OVER_NOTICE_DELAY);
        }
    }

    pub fn on_progress(&mut self, now: Instant, done: u64, total: u64) {
        if !self.active {
            return;
        }
        self.progress = (done, total);
        self.meter.note(now, done);
        self.update_rate(now);
    }

    pub fn on_path(&mut self, direct: Option<bool>) {
        self.route = direct;
    }

    /// What is due at `now`, about once a second: the speed also when no
    /// report comes, and the notice of the hand-over.
    pub fn tick(&mut self, now: Instant) -> Option<Notice> {
        if !self.active {
            return None;
        }
        self.update_rate(now);
        match self.handed_over_notice_at {
            Some(at) if now >= at => {
                self.handed_over_notice_at = None;
                Some(Notice {
                    title: if self.cancel_requested {
                        tr!(notify_too_late_title)
                    } else {
                        tr!(handed_over_title)
                    }
                    .to_string(),
                    text: tr!(handed_over_balloon, item = self.item),
                })
            }
            _ => None,
        }
    }

    /// The speed and the remaining time: only while data moves and some is
    /// still to come, and in a transfer a speed of zero once its data has
    /// stopped coming. The other phases are this computer's own work, whose
    /// reports can pause without anything wrong.
    fn update_rate(&mut self, now: Instant) {
        let (done, total) = self.progress;
        if !moves_data(self.phase) || done >= total {
            self.rate = Rate::Nothing;
            return;
        }
        if self.phase == Some(Phase::Transferring) && self.meter.stalled(now) {
            // Data that comes again is measured anew.
            self.rate_shown = None;
            self.rate = Rate::Stalled;
            return;
        }
        if self
            .rate_shown
            .is_some_and(|t| now.duration_since(t) < RATE_INTERVAL)
        {
            return;
        }
        match self.meter.rate() {
            Some(rate) if rate > 0 => {
                self.rate_shown = Some(now);
                self.rate = Rate::Known {
                    rate,
                    left: (total - done) / rate,
                };
            }
            _ if self.rate_shown.is_none() => self.rate = Rate::Calculating,
            _ => {}
        }
    }

    fn saved_text(&self) -> &'static str {
        match (self.kind, self.received_folder) {
            (Kind::File, _) => tr!(done_saved_file),
            (Kind::Folder, Some(ReceivedFolder::KeepZip)) => tr!(done_saved_folder_zip),
            (Kind::Folder, _) => tr!(done_saved_folder),
        }
    }

    /// The transfer ended. `None` when no notice is needed: the user cancelled
    /// it, and it was cancelled.
    pub fn on_finished(&mut self, outcome: &Outcome) -> Option<Notice> {
        self.active = false;
        self.rate = Rate::Nothing;
        self.handed_over_notice_at = None;
        let file = self.kind == Kind::File;
        let (status, detail, completed) = match (outcome, self.dir) {
            (Outcome::Completed(_), Direction::Send) => {
                let text = if file {
                    tr!(done_sent_file, name = self.peer)
                } else {
                    tr!(done_sent_folder, name = self.peer)
                };
                (text, String::new(), true)
            }
            (Outcome::Completed(path), Direction::Receive) => {
                self.result_path = path.clone();
                let location = path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                (self.saved_text().to_string(), location, true)
            }
            (Outcome::Declined, Direction::Send) => (
                tr!(declined_by_peer, name = self.peer),
                String::new(),
                false,
            ),
            (Outcome::Declined, Direction::Receive) => {
                (tr!(declined).to_string(), String::new(), false)
            }
            (Outcome::Cancelled, _) => (tr!(cancelled).to_string(), String::new(), false),
            (Outcome::CancelledByPeer, _) => (
                tr!(cancelled_by_peer, name = self.peer),
                String::new(),
                false,
            ),
            (Outcome::Unreachable(e), _) => (tr!(unreachable, name = self.peer), e.clone(), false),
            (Outcome::Failed(e), _) => (tr!(failed).to_string(), e.clone(), false),
        };
        self.status = status.clone();
        self.ended_detail = detail;
        self.succeeded = completed;
        if completed {
            self.progress.0 = self.progress.1;
        }
        if self.cancel_requested && matches!(outcome, Outcome::Cancelled) {
            return None;
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
        let text = if completed && self.dir == Direction::Receive {
            os_tr!(notify_saved, item = self.item, name = self.peer)
        } else {
            tr!(notify_status, item = self.item, status = status)
        };
        Some(Notice { title, text })
    }

    fn fraction(&self) -> Option<f64> {
        if self.phase.is_some_and(Self::waits_without_count) && self.active {
            return None;
        }
        let (done, total) = self.progress;
        if total == 0 {
            // Nothing to count yet; once it ended, nothing to wait for.
            return (!self.active).then_some(1.0);
        }
        Some((done as f64 / total as f64).min(1.0))
    }

    pub fn row(&self) -> Row {
        let (done, total) = self.progress;
        let (speed, remaining) = match self.rate {
            Rate::Nothing => (String::new(), String::new()),
            Rate::Calculating => (tr!(calculating).to_string(), tr!(calculating).to_string()),
            Rate::Known { rate, left } => (
                tr!(speed_value, rate = format_size(rate)),
                tr!(remaining_value, left = format_duration(left)),
            ),
            Rate::Stalled => (tr!(speed_value, rate = format_size(0)), String::new()),
        };
        let phase = self.phase.filter(|_| self.active);
        Row {
            id: self.id,
            peer_id: self.peer_id.clone(),
            peer: self.peer.clone(),
            sending: self.dir == Direction::Send,
            item: self.item.clone(),
            active: self.active,
            succeeded: self.succeeded,
            status: self.status.clone(),
            state: if self.active {
                describe(self.phase, self.dir, done, total, &self.item)
            } else {
                self.status.clone()
            },
            fraction: self.fraction(),
            amount: if total > 0 {
                tr!(
                    progress_of,
                    done = format_size(done),
                    total = format_size(total),
                    percent = percent(done, total)
                )
            } else {
                String::new()
            },
            speed,
            remaining,
            detail: if self.active {
                match self.route {
                    Some(true) => tr!(connection_direct).to_string(),
                    Some(false) => tr!(connection_relay).to_string(),
                    None => String::new(),
                }
            } else {
                self.ended_detail.clone()
            },
            can_pause: phase.is_some_and(can_pause) && !self.cancel_requested,
            paused: phase == Some(Phase::Paused),
            can_cancel: self.active && !self.cancel_requested && !self.handed_over,
            path: self.result_path.clone(),
            moving_data: self.active && moves_data(self.phase),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sending(total: u64) -> TransferView {
        TransferView::new(
            1,
            Direction::Send,
            "abc",
            "Ana",
            "report.pdf",
            Kind::File,
            total,
        )
    }

    fn receiving(kind: Kind) -> TransferView {
        TransferView::new(2, Direction::Receive, "abc", "Ana", "a", kind, 100)
    }

    /// The speed comes from the reports of the last few seconds, and a
    /// measurement starts over after a gap; it stalls only once the amount
    /// grew and then did not.
    #[test]
    fn speed_comes_from_the_recent_reports() {
        let mut s = Speed::default();
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_millis(secs * 100);
        s.note(at(0), 0);
        assert!(!s.stalled(at(100)), "nothing has moved yet");
        s.note(at(2), 100);
        assert_eq!(s.rate(), None, "too early");
        s.note(at(20), 200);
        assert_eq!(s.rate(), Some(100));
        s.note(at(70), 700);
        assert_eq!(s.samples.len(), 1, "after a gap");
        assert_eq!(s.rate(), None);
        assert!(!s.stalled(at(70) + STALLED_AFTER - Duration::from_millis(1)));
        assert!(s.stalled(at(70) + STALLED_AFTER));
        s.reset();
        assert!(s.rate().is_none() && !s.stalled(at(200)));
    }

    #[test]
    fn a_lost_connection_is_told_once_data_has_flowed() {
        let lost = Phase::WaitingForConnection { lost: true };
        let mut restored = sending(100);
        assert_eq!(restored.on_phase(lost), None, "it just waits");
        let mut t = sending(100);
        assert_eq!(t.on_phase(Phase::Transferring), None);
        let notice = t.on_phase(lost).unwrap();
        assert_eq!(notice.title, tr!(connection_lost_balloon_title));
        // Moving on says so, once.
        let notice = t.on_phase(Phase::Transferring).unwrap();
        assert_eq!(notice.title, tr!(continuing_balloon_title));
        assert_eq!(t.on_phase(Phase::Transferring), None);
        // The user's own pause is nothing to tell.
        assert_eq!(t.on_phase(Phase::Paused), None);
        assert_eq!(t.on_phase(Phase::Transferring), None);
    }

    /// Checking what arrived after a restart holds nothing up, and does not
    /// clear what did.
    #[test]
    fn verifying_keeps_what_held_the_transfer_up() {
        let mut t = receiving(Kind::File);
        t.on_phase(Phase::Transferring);
        assert!(
            t.on_phase(Phase::WaitingForConnection { lost: true })
                .is_some()
        );
        assert_eq!(t.on_phase(Phase::Verifying), None);
        assert!(t.on_phase(Phase::Transferring).is_some());
    }

    #[test]
    fn another_program_is_told_once_per_hold_up() {
        let mut t = receiving(Kind::File);
        let held = Phase::PartInUse { finishing: false };
        assert!(t.on_phase(held).is_some());
        assert_eq!(t.on_phase(Phase::Paused), None);
        assert_eq!(t.on_phase(held), None);
        t.on_phase(Phase::Transferring);
        assert!(t.on_phase(held).is_some());
    }

    /// A cancel that ends the transfer in a moment gets no notices; one that
    /// turns out to wait gets the one it was spared.
    #[test]
    fn a_cancel_withholds_what_holds_it_up_until_it_waits() {
        let mut t = sending(100);
        t.on_phase(Phase::Transferring);
        t.on_cancel();
        assert_eq!(t.row().status, tr!(cancelling));
        assert!(!t.row().can_cancel && !t.row().can_pause);
        assert_eq!(t.on_phase(Phase::PausedByPeer), None);
        let notice = t.on_cancel_waits().unwrap();
        assert_eq!(notice.title, tr!(paused_balloon_title));
        assert_eq!(t.on_cancel_waits(), None);
        // From now on it is told as it comes.
        assert!(
            t.on_phase(Phase::WaitingForConnection { lost: true })
                .is_some()
        );
    }

    #[test]
    fn a_cancel_that_ended_the_transfer_needs_no_notice() {
        let mut t = sending(100);
        t.on_cancel();
        assert_eq!(t.on_finished(&Outcome::Cancelled), None);
        assert!(!t.row().active && !t.row().succeeded);
        assert_eq!(t.row().status, tr!(cancelled));
    }

    #[test]
    fn a_cancel_that_came_too_late_is_told() {
        let mut t = sending(100);
        t.on_cancel();
        let notice = t.on_finished(&Outcome::Completed(None)).unwrap();
        assert_eq!(notice.title, tr!(notify_too_late_title));
        assert!(t.row().succeeded);
    }

    /// Once the item is the receiver's, Cancel goes away, and a notice comes
    /// a moment later only if the user cancelled or was about to.
    #[test]
    fn the_hand_over_takes_cancel_away() {
        let now = Instant::now();
        let later = now + HANDED_OVER_NOTICE_DELAY;
        let mut t = sending(100);
        t.on_phase(Phase::Transferring);
        assert!(t.row().can_cancel);
        t.on_handed_over(now, false);
        assert!(!t.row().can_cancel);
        assert_eq!(t.row().status, tr!(phase_handed_over, name = "Ana"));
        assert_eq!(t.tick(later), None);

        let mut t = sending(100);
        t.on_phase(Phase::Transferring);
        t.on_handed_over(now, true);
        assert_eq!(t.tick(now), None, "not yet");
        assert_eq!(t.tick(later).unwrap().title, tr!(handed_over_title));
        assert_eq!(t.tick(later), None, "once");

        let mut t = sending(100);
        t.on_cancel();
        t.on_handed_over(now, false);
        assert_eq!(t.tick(later).unwrap().title, tr!(notify_too_late_title));

        // Ended by then: the result says it all.
        let mut t = sending(100);
        t.on_handed_over(now, true);
        t.on_finished(&Outcome::Completed(None));
        assert_eq!(t.tick(later), None);
    }

    #[test]
    fn the_peer_unpacking_tells_why_cancel_goes_away() {
        let mut t = sending(100);
        let notice = t.on_phase(Phase::PeerUnpacking).unwrap();
        assert_eq!(notice.title, tr!(tx_peer_unpacking));
        assert!(!t.row().can_cancel);
        assert_eq!(t.row().fraction, None, "nothing to count here");
        assert_eq!(t.on_phase(Phase::PeerUnpacking), None);
    }

    #[test]
    fn speed_is_shown_only_while_data_moves() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut t = sending(1_000_000);
        t.on_phase(Phase::Transferring);
        t.on_progress(at(0), 0, 1_000_000);
        assert_eq!(t.row().speed, tr!(calculating));
        t.on_progress(at(600), 100_000, 1_000_000);
        assert!(
            t.row().speed.contains(&format_size(166_666)),
            "{}",
            t.row().speed
        );
        assert!(!t.row().remaining.is_empty());
        // Data stops coming: a speed of zero, and no time left to tell.
        t.tick(at(600) + STALLED_AFTER);
        assert_eq!(t.row().speed, tr!(speed_value, rate = format_size(0)));
        assert_eq!(t.row().remaining, "");
        // A pause: nothing to say about speed.
        t.on_phase(Phase::Paused);
        assert_eq!((t.row().speed.as_str(), t.row().paused), ("", true));
        assert!(t.row().can_pause);
        // All there.
        t.on_phase(Phase::Transferring);
        t.on_progress(at(9000), 1_000_000, 1_000_000);
        assert_eq!(t.row().speed, "");
    }

    #[test]
    fn progress_stays_through_a_pause_and_starts_over_for_other_work() {
        let now = Instant::now();
        let mut t = sending(100);
        t.on_phase(Phase::Transferring);
        t.on_progress(now, 40, 100);
        assert_eq!(t.row().fraction, Some(0.4));
        t.on_phase(Phase::Paused);
        assert_eq!(t.row().fraction, Some(0.4));
        t.on_phase(Phase::Transferring);
        assert_eq!(t.row().fraction, Some(0.4));
        t.on_phase(Phase::Verifying);
        assert_eq!(t.row().fraction, Some(0.0));
        assert_eq!(
            t.row().amount,
            tr!(
                progress_of,
                done = format_size(0),
                total = format_size(100),
                percent = 0
            )
        );
    }

    #[test]
    fn the_end_says_where_the_item_went_or_what_failed() {
        let mut t = receiving(Kind::Folder);
        t.set_received_folder(ReceivedFolder::KeepZip);
        let path = PathBuf::from("/Users/ana/Downloads/a.zip");
        let notice = t
            .on_finished(&Outcome::Completed(Some(path.clone())))
            .unwrap();
        assert_eq!(notice.title, tr!(notify_done_title));
        let row = t.row();
        assert_eq!(row.status, tr!(done_saved_folder_zip));
        assert_eq!(row.path, Some(path));
        assert_eq!(row.detail, "/Users/ana/Downloads/a.zip");
        assert_eq!(row.fraction, Some(1.0));

        let mut t = sending(100);
        let notice = t.on_finished(&Outcome::Failed("no room".into())).unwrap();
        assert_eq!(notice.title, tr!(notify_failed_title));
        assert_eq!(
            (t.row().status.as_str(), t.row().detail.as_str()),
            (tr!(failed), "no room")
        );
        assert!(!t.row().can_cancel && !t.row().moving_data);
    }

    #[test]
    fn only_work_on_data_keeps_the_computer_awake() {
        let mut t = sending(100);
        assert!(!t.row().moving_data);
        for (phase, moving) in [
            (Phase::Preparing, true),
            (Phase::Connecting, false),
            (Phase::WaitingForAccept, false),
            (Phase::Transferring, true),
            (Phase::Paused, false),
            (Phase::Verifying, true),
            (Phase::Unpacking, true),
        ] {
            t.on_phase(phase);
            assert_eq!(t.row().moving_data, moving, "{phase:?}");
        }
    }

    #[test]
    fn describing_a_transfer() {
        let line = describe(Some(Phase::Transferring), Direction::Send, 45, 100, "r.pdf");
        assert!(line.contains("45") && line.contains("r.pdf"), "{line}");
        let line = describe(Some(Phase::Connecting), Direction::Send, 0, 100, "r.pdf");
        assert!(!line.contains('%'), "{line}");
    }
}
