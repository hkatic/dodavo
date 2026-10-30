//! Progress window shown for every transfer (sending and receiving).

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
        WindowsAndMessaging::*,
    },
};

use super::{
    ask, confirm, format_duration, format_size, send, set_text, shell, tray,
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    config::CloseAction,
    i18n::tr,
    net::{Command, Kind, Outcome, Phase},
};

const ID_CANCEL: u16 = 11;
const RANGE: usize = 1000;
/// Posted to itself after the transfer ended, to ask or close outside of event handling.
const WM_APP_FINISHED: u32 = WM_APP + 10;
/// Command links of the questions.
const CHOICE_BACKGROUND: i32 = 100;
const CHOICE_CANCEL: i32 = 101;
const CHOICE_OPEN_FILE: i32 = 102;
const CHOICE_OPEN_FOLDER: i32 = 103;
/// How often the speed and the remaining time change, so they can be read.
const RATE_INTERVAL: Duration = Duration::from_secs(1);

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
    phase: Option<Phase>,
    active: bool,
    /// The window was hidden and the transfer continues without it.
    background: bool,
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

pub fn open(
    id: u64,
    dir: Direction,
    peer: &str,
    item: &str,
    kind: Kind,
    size: Option<u64>,
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
        phase: None,
        active: true,
        background: false,
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
        cancel: std::ptr::null_mut(),
    };
    let hwnd = window::create(
        CreateOptions {
            title: &window_title,
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            client: (460, 238),
            center_on: app::main_window(),
            ..Default::default()
        },
        Box::new(handler),
    );
    if let Some(size) = size {
        window::with_handler::<Progress, _>(hwnd, |p, _| p.set_progress(0, size));
    }
    let main_visible = unsafe { IsWindowVisible(app::main_window()) } != 0;
    unsafe {
        // Transfers that start by themselves (incoming) should not steal the focus.
        ShowWindow(
            hwnd,
            if dir == Direction::Send || main_visible {
                SW_SHOW
            } else {
                SW_SHOWNOACTIVATE
            },
        );
    }
    hwnd
}

pub fn is_active(hwnd: HWND) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.active).unwrap_or(true)
}

pub fn set_phase(hwnd: HWND, phase: Phase) -> bool {
    window::with_handler::<Progress, _>(hwnd, |p, _| p.set_phase(phase)).is_some()
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

impl Progress {
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

    fn clear_rows(&mut self) {
        for h in [self.done, self.speed, self.remaining] {
            set_text(h, "");
        }
        self.samples.clear();
        self.rate_shown = None;
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = Some(phase);
        let text = match phase {
            Phase::Preparing => tr!(phase_preparing).to_string(),
            Phase::Connecting => tr!(phase_connecting, name = self.peer),
            Phase::WaitingForAccept => tr!(phase_waiting, name = self.peer),
            Phase::Transferring => match self.dir {
                Direction::Send => tr!(phase_sending).to_string(),
                Direction::Receive => tr!(phase_receiving).to_string(),
            },
            Phase::Unpacking => tr!(phase_unpacking).to_string(),
        };
        set_text(self.status, &text);
        set_text(
            self.done_caption,
            match phase {
                Phase::Preparing | Phase::Unpacking => tr!(row_processed),
                _ => tr!(row_transferred),
            },
        );
        let waiting = matches!(phase, Phase::Connecting | Phase::WaitingForAccept);
        self.marquee(waiting);
        if !waiting {
            send(self.bar, PBM_SETPOS, 0, 0);
        }
        self.clear_rows();
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
                let text = if file {
                    tr!(done_saved_file)
                } else {
                    tr!(done_saved_folder)
                };
                (text.to_string(), location)
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
        set_text(self.cancel, tr!(close));
        window::enable(self.cancel, true);

        let received = completed && self.dir == Direction::Receive;
        let will_ask = received && app::cfg().ask_after_receive;
        if self.background || received {
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
        // The question asked after a receive is announced by itself; every
        // other outcome is reported with a notification too.
        Finished {
            title,
            text,
            path: self.result_path.clone(),
            notify: self.background || !will_ask,
        }
    }

    fn cancel_transfer(&mut self) {
        app::net_send(Command::Cancel { id: self.id });
        set_text(self.status, tr!(cancelling));
        window::enable(self.cancel, false);
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
            self.background = true;
            window::show(w.hwnd, false);
            tray::balloon(
                app::main_window(),
                tr!(background_balloon_title),
                &tr!(background_balloon, item = self.item),
                None,
            );
        }
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
        let file = self.kind == Kind::File;
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
            if file {
                tr!(done_saved_file)
            } else {
                tr!(done_saved_folder)
            },
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
        let title = match self.dir {
            Direction::Send => tr!(to_peer, name = self.peer, item = self.item),
            Direction::Receive => tr!(from_peer, name = self.peer, item = self.item),
        };
        self.title = w.label(&title, (14, 12, 432, 20));
        w.make_bold(self.title);
        self.status = w.label(tr!(starting), (14, 38, 432, 36));
        self.bar = w.add("msctls_progress32", "", 0, 0, (14, 78, 432, 16));
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
        let value_w = 432 - caption_w;
        let row = |caption: &str, y: i32| {
            let c = w.label(caption, (14, y, caption_w, 18));
            (c, w.label("", (value_x, y, value_w, 18)))
        };
        (self.done_caption, self.done) = row(tr!(row_transferred), 104);
        self.speed = row(tr!(row_speed), 124).1;
        self.remaining = row(tr!(row_remaining), 144).1;

        self.connection = w.label("", (14, 168, 432, 34));
        // Right-aligned button, as wide as its (translated) texts need.
        let cancel_w = w
            .button_width(tr!(cancel), 92)
            .max(w.button_width(tr!(close), 92));
        self.cancel = w.button(tr!(cancel), ID_CANCEL, (446 - cancel_w, 200, cancel_w, 28));
        w.default_id.set(ID_CANCEL);
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        match id {
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

    fn on_message(&mut self, w: &Window, msg: u32, _wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        if msg != WM_APP_FINISHED {
            return None;
        }
        if self.background {
            w.close();
        } else {
            self.after_received(w);
        }
        Some(0)
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
