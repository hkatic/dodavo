//! The "Update Available" window, and the UI side of update checks: when
//! they run, and what happens with their results. The checks and downloads
//! themselves are in `crate::update`.
//!
//! Accessibility: the window is announced through a notification when it
//! appears by itself (it is shown without taking the focus). It has the
//! dialog role, so when it is activated screen readers read the headline,
//! as in a message box, and then the release notes, which have the focus
//! (the user reads them before choosing). Starting the download moves the
//! focus to Cancel Download, which screen readers announce; the status line
//! is a live region that announces the start of Setup, but not the progress
//! (it is readable like in the progress window) or the check of the file,
//! which takes no time; failures are message boxes.

use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::{
    Foundation::{HANDLE, HWND},
    UI::{
        Controls::{PBM_SETPOS, PBM_SETRANGE32},
        WindowsAndMessaging::{
            IsWindowVisible, KillTimer, SW_SHOW, SW_SHOWNOACTIVATE, SetForegroundWindow, SetTimer,
            ShowWindow, WM_TIMER, WS_CAPTION, WS_CLIPCHILDREN, WS_EX_APPWINDOW,
            WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
        },
    },
};

use super::{
    accessibility, confirm, error, format_size, info, main_window, send, set_text, shell,
    tray::{self, BalloonAction},
    window::{self, CreateOptions, Handler, Window},
};
use crate::{
    app,
    i18n::{self, tr},
    update::{self, Error, Event, Job, Trigger, UpdateInfo, version::Version},
};

const ID_NOTES: u16 = 10;
const ID_INSTALL: u16 = 11;
const ID_LATER: u16 = 12;
const ID_CANCEL_DOWNLOAD: u16 = 13;
const ID_BAR: u16 = 14;
/// Polls whether Setup ended early after it was started.
const TIMER_LAUNCH: usize = 1;
const LAUNCH_CHECK_MS: u32 = 500;
const RANGE: u64 = 1000;
const WIDTH: i32 = 520;
const HEIGHT: i32 = 358;
/// The download needs the installer once, Setup unpacks it once more.
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

enum State {
    Idle,
    Downloading,
    Verifying,
    Launching { process: HANDLE },
}

struct UpdateWindow {
    info: UpdateInfo,
    state: State,
    /// Answered before the download when transfers were running:
    /// `Some(true)` cancels them when Setup takes over, `Some(false)` suspends them.
    cancel_transfers: Option<bool>,
    notes: HWND,
    status: HWND,
    bar: HWND,
    install: HWND,
    later: HWND,
    cancel_download: HWND,
    /// The status text is rewritten at most once a second.
    last_status: Option<Instant>,
    /// Closing the window means "remind me later", unless Setup took over.
    snooze_on_close: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Opens the window for a newer version. Started by the user (`Manual`), it
/// takes the focus; otherwise it appears in the background with a notification.
pub fn open(info: UpdateInfo, trigger: Trigger) -> HWND {
    let version = info.version.clone();
    let handler = UpdateWindow {
        info,
        state: State::Idle,
        cancel_transfers: None,
        notes: std::ptr::null_mut(),
        status: std::ptr::null_mut(),
        bar: std::ptr::null_mut(),
        install: std::ptr::null_mut(),
        later: std::ptr::null_mut(),
        cancel_download: std::ptr::null_mut(),
        last_status: None,
        snooze_on_close: true,
    };
    let hwnd = window::create(
        CreateOptions {
            title: tr!(update_title),
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_APPWINDOW,
            client: (WIDTH, HEIGHT),
            center_on: app::main_window(),
            ..Default::default()
        },
        Box::new(handler),
    );
    app::with(|a| a.update_window = hwnd);
    match trigger {
        Trigger::Manual => unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        },
        Trigger::Automatic => {
            unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            tray::balloon(
                app::main_window(),
                tr!(update_balloon_title),
                &tr!(update_balloon, version = version),
                Some(BalloonAction::Update),
            );
        }
    }
    hwnd
}

/// Called right after the main window exists. After an update, says so;
/// otherwise reports an update that was handed to Setup but never happened.
pub fn on_start(updated: bool) {
    let main = app::main_window();
    if updated {
        update::remove_pending();
        tray::balloon(
            main,
            tr!(updated_balloon_title),
            &tr!(updated_balloon, version = env!("CARGO_PKG_VERSION")),
            None,
        );
    } else if let Some(version) = update::read_pending() {
        update::remove_pending();
        if Version::parse(&version).is_some_and(|v| v > Version::current()) {
            info(main, &tr!(update_not_installed, version = version));
        }
    }
}

/// Starts the daily check when it is due (called by the main window's
/// timers and when the network comes online).
pub fn tick() {
    let cfg = app::cfg();
    let (online, busy) = app::with(|a| {
        (
            a.online,
            a.update_job.is_some() || !a.update_window.is_null(),
        )
    });
    if !cfg.check_updates || online != Some(true) || busy {
        return;
    }
    let now = now_secs();
    // A last check in the future means the clock was changed: check again.
    let recent = cfg
        .update_last_check
        .is_some_and(|t| t <= now && now < t + update::INTERVAL.as_secs());
    let snoozed = cfg.update_remind_after.is_some_and(|t| now < t);
    if recent || snoozed {
        return;
    }
    start_check(Trigger::Automatic);
}

/// Help → Check for Updates.
pub fn check_now() {
    let (hwnd, running) = app::with(|a| (a.update_window, a.update_job.is_some()));
    if !hwnd.is_null() {
        window::bring_to_front(hwnd);
        return;
    }
    main_window::set_status(tr!(update_checking));
    if running {
        // The daily check is already on its way: report its result instead.
        // The status bar says that it is checking; a message box here would
        // still be open when the answer opens its own.
        app::with(|a| {
            if let Some(job) = &mut a.update_job {
                job.trigger = Trigger::Manual;
            }
        });
        return;
    }
    start_check(Trigger::Manual);
}

fn start_check(trigger: Trigger) {
    let job = Job::new(trigger);
    app::with(|a| a.update_job = Some(job.clone()));
    update::spawn_check(job, i18n::lang().code().to_string(), app::update_sink);
}

/// Delivers an event of the update thread; returns it if the window is busy.
pub fn handle_event(event: Event) -> Option<Event> {
    let (job, hwnd) = app::with(|a| {
        (
            a.update_job.as_ref().map(|j| (j.id, j.trigger)),
            a.update_window,
        )
    });
    let Some((job_id, trigger)) = job else {
        // A cancelled or forgotten job.
        return None;
    };
    if event.job() != job_id {
        return None;
    }
    match event {
        Event::Checked { result, .. } => {
            app::with(|a| a.update_job = None);
            checked(trigger, result);
            None
        }
        Event::Progress { done, total, .. } => {
            // Frequent; a dropped one does no harm.
            window::with_handler::<UpdateWindow, _>(hwnd, |u, _| u.progress(done, total));
            None
        }
        Event::Verifying { job } => {
            window::with_handler::<UpdateWindow, _>(hwnd, |u, _| u.verifying())
                .map_or(Some(Event::Verifying { job }), |_| None)
        }
        Event::Downloaded { job, result } => {
            let mut slot = Some(result);
            let delivered = window::with_handler::<UpdateWindow, _>(hwnd, |u, w| {
                app::with(|a| a.update_job = None);
                u.downloaded(w, slot.take().expect("result"));
            });
            match (delivered, slot) {
                (None, Some(result)) if !hwnd.is_null() => Some(Event::Downloaded { job, result }),
                _ => None,
            }
        }
    }
}

/// The outcome of a check.
fn checked(trigger: Trigger, result: Result<Option<UpdateInfo>, Error>) {
    let main = app::main_window();
    if trigger == Trigger::Manual {
        main_window::show_status();
    }
    match result {
        Ok(found) => {
            app::update_cfg(|c| c.update_last_check = Some(now_secs()));
            match found {
                Some(info) => {
                    let existing = app::with(|a| a.update_window);
                    if existing.is_null() {
                        open(info, trigger);
                    } else if trigger == Trigger::Manual {
                        window::bring_to_front(existing);
                    }
                }
                None => {
                    if trigger == Trigger::Manual {
                        info(
                            main,
                            &tr!(update_latest, version = env!("CARGO_PKG_VERSION")),
                        );
                    }
                }
            }
        }
        Err(e) => {
            if trigger == Trigger::Manual {
                error(main, &tr!(update_check_failed, error = e.text()));
            }
        }
    }
}

impl UpdateWindow {
    fn install(&mut self, w: &Window) {
        if app::active_transfer_count() > 0 {
            match main_window::confirm_exit(w.hwnd) {
                Some(cancel) => self.cancel_transfers = Some(cancel),
                None => return,
            }
        }
        let dir = match update::update_dir() {
            Ok(dir) => dir,
            Err(e) => {
                self.fail(w, Error::io(&e));
                return;
            }
        };
        if let Some((_, free)) = shell::disk_free(&dir) {
            let needed = self.info.size.saturating_mul(2) + SPACE_MARGIN;
            if free < needed {
                self.fail(w, Error::DiskSpace { needed, free });
                return;
            }
        }
        update::cleanup();
        let job = Job::new(Trigger::Manual);
        app::with(|a| a.update_job = Some(job.clone()));
        update::spawn_download(
            job,
            self.info.clone(),
            dir.join(&self.info.file_name),
            app::update_sink,
        );
        self.state = State::Downloading;
        self.last_status = None;
        self.show_download_progress(0, self.info.size);
        window::show(self.status, true);
        window::show(self.bar, true);
        // The focus moves before the old buttons go, so it never gets lost.
        w.default_id.set(0);
        window::show(self.cancel_download, true);
        window::focus(self.cancel_download);
        window::show(self.install, false);
        window::show(self.later, false);
    }

    fn show_download_progress(&mut self, done: u64, total: u64) {
        let pos = (done * RANGE).checked_div(total).unwrap_or(0);
        send(self.bar, PBM_SETPOS, pos as usize, 0);
        let due = self
            .last_status
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            || done == total;
        if due {
            self.last_status = Some(Instant::now());
            set_text(
                self.status,
                &tr!(
                    update_downloading,
                    done = format_size(done),
                    total = format_size(total),
                    percent = pos / 10
                ),
            );
        }
    }

    fn progress(&mut self, done: u64, total: u64) {
        if matches!(self.state, State::Downloading) {
            self.show_download_progress(done, total);
        }
    }

    fn verifying(&mut self) {
        if matches!(self.state, State::Downloading) {
            self.state = State::Verifying;
            send(self.bar, PBM_SETPOS, RANGE as usize, 0);
            set_text(self.status, tr!(update_verifying));
        }
    }

    fn downloaded(&mut self, w: &Window, result: Result<PathBuf, Error>) {
        match result {
            Ok(setup) => self.launch(w, setup),
            // Our own cancellation; the window has already moved on.
            Err(Error::Cancelled) => {}
            Err(e) => self.fail(w, e),
        }
    }

    /// Back to the buttons; `status` stays as the explanation.
    fn back_to_idle(&mut self, w: &Window, status: Option<&str>) {
        self.state = State::Idle;
        match status {
            Some(text) => set_text(self.status, text),
            None => window::show(self.status, false),
        }
        window::show(self.bar, false);
        window::show(self.install, true);
        window::show(self.later, true);
        window::focus(self.install);
        window::show(self.cancel_download, false);
        window::enable(self.cancel_download, true);
        w.default_id.set(ID_INSTALL);
    }

    fn fail(&mut self, w: &Window, e: Error) {
        // The choice was for this attempt, not for a later one from elsewhere.
        app::with(|a| a.update_cancel_transfers = false);
        let text = e.text();
        self.back_to_idle(w, Some(&text));
        window::show(self.status, true);
        error(w.hwnd, &tr!(update_failed, error = text));
    }

    /// Asks, then stops the download. Returns whether it was stopped.
    fn cancel_download(&mut self, w: &Window) -> bool {
        if !confirm(w.hwnd, tr!(update_cancel_confirm)) {
            return false;
        }
        // Events of this job are ignored from now on; the thread removes
        // the partial file.
        app::with(|a| {
            if let Some(job) = a.update_job.take() {
                job.cancel.cancel();
            }
        });
        self.back_to_idle(w, None);
        true
    }

    /// Starts Setup. From here on, the main window's handler of
    /// `Dodavo_ExitForUpdate` ends the program when Setup asks for it; this
    /// window only watches whether Setup ends early instead.
    fn launch(&mut self, w: &Window, setup: PathBuf) {
        let mode = update::install_mode();
        // Cancel Download can no longer be used; the focus moves before it
        // is disabled, so it never gets lost, and before the announcement,
        // which a change of focus would interrupt.
        window::focus(self.notes);
        window::enable(self.cancel_download, false);
        set_text(
            self.status,
            if mode == update::InstallMode::AllUsers {
                tr!(update_launching_admin)
            } else {
                tr!(update_launching)
            },
        );
        accessibility::announce(self.status);
        let main = app::main_window();
        let mut args = String::from("--updated");
        if let Some(profile) = app::with(|a| a.profile.clone()) {
            args.push_str(" --profile ");
            args.push_str(&profile);
        }
        if unsafe { IsWindowVisible(main) } == 0 {
            args.push_str(" --minimized");
        }
        let cancel_transfers = self.cancel_transfers.unwrap_or(false);
        app::with(|a| a.update_cancel_transfers = cancel_transfers);
        let _ = update::write_pending(&self.info.version);
        match update::launch_installer(&setup, mode, &args) {
            Ok(process) => {
                self.snooze_on_close = false;
                self.state = State::Launching { process };
                unsafe { SetTimer(w.hwnd, TIMER_LAUNCH, LAUNCH_CHECK_MS, None) };
            }
            Err(e) => {
                update::remove_pending();
                self.fail(w, e);
            }
        }
    }

    /// Setup ended before it asked us to exit: it was declined the elevation
    /// prompt, or it gave up waiting. Report it and offer to try again.
    fn launch_tick(&mut self, w: &Window) {
        let State::Launching { process } = self.state else {
            return;
        };
        let Some(code) = update::setup_exited(process) else {
            return;
        };
        unsafe { KillTimer(w.hwnd, TIMER_LAUNCH) };
        update::close_handle(process);
        update::remove_pending();
        self.snooze_on_close = true;
        self.fail(w, Error::SetupExit(code));
    }
}

impl Handler for UpdateWindow {
    fn on_create(&mut self, w: &Window) {
        w.set_dialog_role();
        let headline = w.label(
            &tr!(
                update_headline,
                version = self.info.version,
                current = env!("CARGO_PKG_VERSION")
            ),
            (14, 12, WIDTH - 28, 20),
        );
        w.make_bold(headline);
        // The caption names the notes for screen readers.
        w.caption(tr!(release_notes_label), (14, 40, WIDTH - 28, 18));
        self.notes = w.read_only_text(&self.info.notes, ID_NOTES, (14, 60, WIDTH - 28, 190));
        self.status = w.label("", (14, 262, WIDTH - 28, 18));
        w.live_region(self.status);
        self.bar = w.add(
            "msctls_progress32",
            "",
            0,
            ID_BAR,
            (14, 284, WIDTH - 28, 16),
        );
        // Otherwise named after the status line, which screen readers would
        // then leave out when they read the window.
        w.set_accessible_name(self.bar, tr!(progress_bar));
        send(self.bar, PBM_SETRANGE32, 0, RANGE as isize);
        window::show(self.status, false);
        window::show(self.bar, false);

        let button_w = [tr!(install_update), tr!(remind_later), tr!(cancel_download)]
            .iter()
            .map(|t| w.button_width(t, 110))
            .max()
            .unwrap_or(110);
        let right = WIDTH - 14;
        self.install = w.button(
            tr!(install_update),
            ID_INSTALL,
            (right - 2 * button_w - 6, 314, button_w, 30),
        );
        self.later = w.button(
            tr!(remind_later),
            ID_LATER,
            (right - button_w, 314, button_w, 30),
        );
        self.cancel_download = w.button(
            tr!(cancel_download),
            ID_CANCEL_DOWNLOAD,
            (right - button_w, 314, button_w, 30),
        );
        window::show(self.cancel_download, false);
        w.default_id.set(ID_INSTALL);
        // The user reads what is new before choosing; Enter still installs.
        window::focus(self.notes);
    }

    fn on_command(&mut self, w: &Window, id: u16, _code: u16) -> bool {
        match id {
            ID_INSTALL if matches!(self.state, State::Idle) => self.install(w),
            ID_LATER if matches!(self.state, State::Idle) => w.close(),
            ID_CANCEL_DOWNLOAD if matches!(self.state, State::Downloading | State::Verifying) => {
                self.cancel_download(w);
            }
            _ => return false,
        }
        true
    }

    fn on_close(&mut self, w: &Window) {
        match self.state {
            State::Idle => w.close(),
            State::Downloading | State::Verifying => {
                if self.cancel_download(w) {
                    w.close();
                }
            }
            // Setup is about to take over.
            State::Launching { .. } => {}
        }
    }

    fn on_message(&mut self, w: &Window, msg: u32, wp: usize, _lp: isize) -> Option<isize> {
        if msg == WM_TIMER && wp == TIMER_LAUNCH {
            self.launch_tick(w);
            return Some(0);
        }
        None
    }

    fn on_destroy(&mut self, _w: &Window) {
        app::with(|a| {
            if let Some(job) = a.update_job.take() {
                job.cancel.cancel();
            }
            a.update_window = std::ptr::null_mut();
        });
        if self.snooze_on_close {
            let until = now_secs() + update::SNOOZE.as_secs();
            app::update_cfg(|c| c.update_remind_after = Some(until));
        }
    }
}
