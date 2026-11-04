//! A small framework over raw Win32 windows: every window owns a [`Handler`]
//! with its logic, controls are positioned in DPI-independent units, and
//! theming (light/dark), fonts, DPI changes and keyboard navigation are
//! handled uniformly.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    ffi::{CString, c_void},
    sync::{Once, OnceLock},
};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM},
    Graphics::Gdi::{
        CreateFontIndirectW, DT_CALCRECT, DT_EXPANDTABS, DT_NOPREFIX, DT_WORDBREAK, DeleteObject,
        DrawTextW, FW_BOLD, FillRect, GetDC, GetDeviceCaps, GetMonitorInfoW, GetTextExtentPoint32W,
        HDC, HFONT, LOGPIXELSX, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        ReleaseDC, SelectObject,
    },
    System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    UI::{
        Controls::NMHDR,
        Input::KeyboardAndMouse::{
            EnableWindow, GetFocus, IsWindowEnabled, SetActiveWindow, SetFocus,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

use super::{accessibility, theme, wide};

pub const MAIN_CLASS: &str = "Dodavo_Main";
const WINDOW_CLASS: &str = "Dodavo_Window";
const DM_GETDEFID: u32 = WM_USER;
const DC_HASDEFID: isize = 0x534B;
const SS_LEFT: u32 = 0;
const SS_NOPREFIX: u32 = 0x80;

/// A control whose position is given in 96-DPI units and scaled automatically.
struct Ctl {
    hwnd: HWND,
    rect: (i32, i32, i32, i32),
}

pub struct Window {
    pub hwnd: HWND,
    /// The main window, as opposed to a dialog-like window that Escape closes.
    is_main: bool,
    dpi: Cell<u32>,
    font: Cell<HFONT>,
    bold_font: Cell<HFONT>,
    ctls: RefCell<Vec<Ctl>>,
    bold_ctls: RefCell<Vec<HWND>>,
    /// Command id triggered by the Enter key.
    pub default_id: Cell<u16>,
    /// For modal windows: the window that was disabled while this one is open.
    modal_owner: Cell<HWND>,
    /// The control that gets the focus back when the window is activated again.
    last_focus: Cell<HWND>,
    /// The window and controls with screen reader annotations, cleared when
    /// the window is destroyed.
    annotated: RefCell<Vec<HWND>>,
}

pub trait Handler: Any {
    /// Create the controls here.
    fn on_create(&mut self, _w: &Window) {}
    /// Return `true` if the command was handled.
    fn on_command(&mut self, _w: &Window, _id: u16, _code: u16) -> bool {
        false
    }
    fn on_notify(&mut self, _w: &Window, _hdr: &NMHDR, _lparam: LPARAM) -> Option<LRESULT> {
        None
    }
    /// Called after resizing (and after DPI changes); `cx`/`cy` are in 96-DPI units.
    fn on_size(&mut self, _w: &Window, _cx: i32, _cy: i32) {}
    fn on_close(&mut self, w: &Window) {
        w.close();
    }
    fn on_message(&mut self, _w: &Window, _msg: u32, _wp: WPARAM, _lp: LPARAM) -> Option<LRESULT> {
        None
    }
    fn on_destroy(&mut self, _w: &Window) {}
}

/// Handles a window message without the window's handler; see
/// [`CreateOptions::urgent`].
pub type UrgentFn = fn(&Window, u32, WPARAM, LPARAM) -> Option<LRESULT>;

struct State {
    win: Window,
    handler: RefCell<Box<dyn Handler>>,
    urgent: Option<UrgentFn>,
    depth: Cell<u32>,
    dead: Cell<bool>,
}

thread_local! {
    static TOP_LEVEL: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

type RawProc = unsafe extern "system" fn() -> isize;
type GetDpiForWindowFn = unsafe extern "system" fn(HWND) -> u32;
type SystemParametersInfoForDpiFn =
    unsafe extern "system" fn(u32, u32, *mut c_void, u32, u32) -> i32;
type AdjustWindowRectExForDpiFn = unsafe extern "system" fn(*mut RECT, u32, i32, u32, u32) -> i32;

/// The per-window DPI functions, which exist from Windows 10 version 1607 on.
/// They are looked up at run time (never imported statically) so that the
/// program also starts on Windows 7, 8 and 8.1, where every window uses the
/// system DPI and the fallbacks below are exact.
struct DpiApi {
    get_dpi_for_window: Option<GetDpiForWindowFn>,
    system_parameters_info_for_dpi: Option<SystemParametersInfoForDpiFn>,
    adjust_window_rect_ex_for_dpi: Option<AdjustWindowRectExForDpiFn>,
}

fn dpi_api() -> &'static DpiApi {
    static API: OnceLock<DpiApi> = OnceLock::new();
    API.get_or_init(|| {
        // user32 is loaded for the lifetime of the process; nothing to free.
        let user32 = unsafe { GetModuleHandleW(wide("user32.dll").as_ptr()) };
        // DODAVO_LEGACY_DPI=1 pretends the functions are missing, to test the
        // Windows 7/8 code path on a current Windows.
        let legacy = std::env::var_os("DODAVO_LEGACY_DPI").is_some_and(|v| v == "1");
        let find = |name: &str| -> Option<RawProc> {
            if user32.is_null() || legacy {
                return None;
            }
            let name = CString::new(name).expect("static name");
            unsafe { GetProcAddress(user32, name.as_ptr() as *const u8) }
        };
        // SAFETY: each pointer is transmuted to the documented signature of
        // the function it was looked up by name.
        unsafe {
            DpiApi {
                get_dpi_for_window: find("GetDpiForWindow")
                    .map(|f| std::mem::transmute::<RawProc, GetDpiForWindowFn>(f)),
                system_parameters_info_for_dpi: find("SystemParametersInfoForDpi")
                    .map(|f| std::mem::transmute::<RawProc, SystemParametersInfoForDpiFn>(f)),
                adjust_window_rect_ex_for_dpi: find("AdjustWindowRectExForDpi")
                    .map(|f| std::mem::transmute::<RawProc, AdjustWindowRectExForDpiFn>(f)),
            }
        }
    })
}

/// The system DPI (that of the primary monitor), which Windows before 8.1
/// uses for every window.
fn system_dpi() -> u32 {
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let dpi = GetDeviceCaps(dc, LOGPIXELSX as i32);
        ReleaseDC(std::ptr::null_mut(), dc);
        dpi.max(96) as u32
    }
}

/// The DPI a window is drawn at.
fn window_dpi(hwnd: HWND) -> u32 {
    match dpi_api().get_dpi_for_window {
        Some(f) => unsafe { f(hwnd) }.max(96),
        None => system_dpi(),
    }
}

/// All top-level windows created by the framework.
pub fn all_windows() -> Vec<HWND> {
    TOP_LEVEL.with(|t| t.borrow().clone())
}

fn register_classes() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let icon = LoadIconW(hinst, 1 as _);
        let small = LoadImageW(
            hinst,
            1 as _,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        );
        for name in [MAIN_CLASS, WINDOW_CLASS] {
            let class_name = wide(name);
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                hIcon: icon,
                hIconSm: small as _,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class_name.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
            // The class name buffer only needs to live during registration.
        }
    });
}

pub struct CreateOptions<'a> {
    pub title: &'a str,
    pub main_class: bool,
    pub style: WINDOW_STYLE,
    pub ex_style: WINDOW_EX_STYLE,
    pub menu: HMENU,
    /// Client area size in 96-DPI units.
    pub client: (i32, i32),
    /// Window to center on (`null` = center on screen, `None`-like behaviour).
    pub center_on: HWND,
    /// Sees every message before the handler, and also while the handler is
    /// busy: a modal window, message box or task dialog it opened runs its
    /// own message loop, and the messages the window gets meanwhile would
    /// otherwise go to `DefWindowProcW`. For messages that must not be lost
    /// and need nothing of the handler. Returns `Some` if it handled the
    /// message.
    pub urgent: Option<UrgentFn>,
}

impl Default for CreateOptions<'_> {
    fn default() -> Self {
        CreateOptions {
            title: "",
            main_class: false,
            // A fixed-size dialog-like window.
            style: WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
            ex_style: WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME,
            menu: std::ptr::null_mut(),
            client: (400, 200),
            center_on: std::ptr::null_mut(),
            urgent: None,
        }
    }
}

/// Creates (but does not show) a top-level window driven by `handler`.
pub fn create(opts: CreateOptions, handler: Box<dyn Handler>) -> HWND {
    register_classes();
    let state = Box::new(State {
        win: Window {
            hwnd: std::ptr::null_mut(),
            is_main: opts.main_class,
            dpi: Cell::new(96),
            font: Cell::new(std::ptr::null_mut()),
            bold_font: Cell::new(std::ptr::null_mut()),
            ctls: RefCell::new(Vec::new()),
            bold_ctls: RefCell::new(Vec::new()),
            default_id: Cell::new(0),
            modal_owner: Cell::new(std::ptr::null_mut()),
            last_focus: Cell::new(std::ptr::null_mut()),
            annotated: RefCell::new(Vec::new()),
        },
        handler: RefCell::new(handler),
        urgent: opts.urgent,
        depth: Cell::new(0),
        dead: Cell::new(false),
    });
    let class = wide(if opts.main_class {
        MAIN_CLASS
    } else {
        WINDOW_CLASS
    });
    let hwnd = unsafe {
        CreateWindowExW(
            opts.ex_style,
            class.as_ptr(),
            wide(opts.title).as_ptr(),
            opts.style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            std::ptr::null_mut(),
            opts.menu,
            GetModuleHandleW(std::ptr::null()),
            Box::into_raw(state) as *const _,
        )
    };
    assert!(!hwnd.is_null(), "CreateWindowExW failed");
    TOP_LEVEL.with(|t| t.borrow_mut().push(hwnd));

    with_state(hwnd, |s| {
        let w = &s.win;
        w.dpi.set(window_dpi(hwnd));
        w.update_fonts();
        w.set_client_size(opts.client.0, opts.client.1);
        if let Ok(mut h) = s.handler.try_borrow_mut() {
            h.on_create(w);
        }
        w.center(opts.center_on);
        theme::apply_to_window(hwnd);
        w.relayout();
    });
    hwnd
}

/// Runs a nested message loop until `hwnd` is destroyed, disabling `owner`
/// meanwhile. When the program ends meanwhile (WM_QUIT), the window is
/// closed and WM_QUIT passed on to the loop below.
pub fn run_modal(owner: HWND, hwnd: HWND) {
    with_state(hwnd, |s| s.win.modal_owner.set(owner));
    unsafe {
        if !owner.is_null() {
            EnableWindow(owner, 0);
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        let mut msg: MSG = std::mem::zeroed();
        while IsWindow(hwnd) != 0 {
            if GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) <= 0 {
                PostQuitMessage(msg.wParam as i32);
                DestroyWindow(hwnd);
                break;
            }
            if !super::main_window::translate(&msg) {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if !owner.is_null() {
            EnableWindow(owner, 1);
        }
    }
}

/// Gives access to the window (without borrowing its handler).
pub fn with_window<R>(hwnd: HWND, f: impl FnOnce(&Window) -> R) -> Option<R> {
    with_state(hwnd, |s| f(&s.win))
}

/// Gives access to the handler of a window if it is of type `T`.
pub fn with_handler<T: Handler, R>(hwnd: HWND, f: impl FnOnce(&mut T, &Window) -> R) -> Option<R> {
    with_state(hwnd, |s| {
        let mut h = s.handler.try_borrow_mut().ok()?;
        let any: &mut dyn Any = &mut **h;
        any.downcast_mut::<T>().map(|t| f(t, &s.win))
    })
    .flatten()
}

fn with_state<R>(hwnd: HWND, f: impl FnOnce(&State) -> R) -> Option<R> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as usize as *mut State;
    if ptr.is_null() {
        return None;
    }
    let state = unsafe { &*ptr };
    state.depth.set(state.depth.get() + 1);
    let r = f(state);
    leave(ptr);
    Some(r)
}

fn leave(ptr: *mut State) {
    let state = unsafe { &*ptr };
    state.depth.set(state.depth.get() - 1);
    if state.depth.get() == 0 && state.dead.get() {
        let state = unsafe { Box::from_raw(ptr) };
        if let Ok(mut h) = state.handler.try_borrow_mut() {
            h.on_destroy(&state.win);
        }
        state.win.free_fonts();
    }
}

impl Window {
    /// Scales a 96-DPI value to physical pixels.
    pub fn px(&self, v: i32) -> i32 {
        (v as i64 * self.dpi.get() as i64 / 96) as i32
    }

    /// Converts physical pixels to 96-DPI units.
    pub fn dip(&self, v: i32) -> i32 {
        (v as i64 * 96 / self.dpi.get() as i64) as i32
    }

    fn update_fonts(&self) {
        unsafe {
            let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
            ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
            let dpi = self.dpi.get();
            match dpi_api().system_parameters_info_for_dpi {
                Some(f) => {
                    f(
                        SPI_GETNONCLIENTMETRICS,
                        ncm.cbSize,
                        &mut ncm as *mut _ as _,
                        0,
                        dpi,
                    );
                }
                None => {
                    SystemParametersInfoW(
                        SPI_GETNONCLIENTMETRICS,
                        ncm.cbSize,
                        &mut ncm as *mut _ as _,
                        0,
                    );
                    // The metrics are for the system DPI; scale the font to
                    // this window's DPI (they differ only on Windows 8.1).
                    let system = system_dpi();
                    if system != dpi {
                        let h = ncm.lfMessageFont.lfHeight as i64 * dpi as i64 / system as i64;
                        ncm.lfMessageFont.lfHeight = h as i32;
                    }
                }
            }
            self.free_fonts();
            self.font.set(CreateFontIndirectW(&ncm.lfMessageFont));
            let mut bold = ncm.lfMessageFont;
            bold.lfWeight = FW_BOLD as i32;
            self.bold_font.set(CreateFontIndirectW(&bold));
        }
        for c in self.ctls.borrow().iter() {
            send_font(c.hwnd, self.font.get());
        }
        for &c in self.bold_ctls.borrow().iter() {
            send_font(c, self.bold_font.get());
        }
    }

    fn free_fonts(&self) {
        unsafe {
            for f in [
                self.font.replace(std::ptr::null_mut()),
                self.bold_font.replace(std::ptr::null_mut()),
            ] {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
        }
    }

    /// Creates a child control at the given 96-DPI rectangle.
    pub fn add(
        &self,
        class: &str,
        text: &str,
        style: WINDOW_STYLE,
        id: u16,
        rect: (i32, i32, i32, i32),
    ) -> HWND {
        self.add_ex(class, text, style, 0, id, rect)
    }

    pub fn add_ex(
        &self,
        class: &str,
        text: &str,
        style: WINDOW_STYLE,
        ex_style: WINDOW_EX_STYLE,
        id: u16,
        rect: (i32, i32, i32, i32),
    ) -> HWND {
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                self.px(rect.0),
                self.px(rect.1),
                self.px(rect.2),
                self.px(rect.3),
                self.hwnd,
                id as usize as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        send_font(hwnd, self.font.get());
        self.ctls.borrow_mut().push(Ctl { hwnd, rect });
        hwnd
    }

    pub fn make_bold(&self, ctl: HWND) {
        send_font(ctl, self.bold_font.get());
        self.bold_ctls.borrow_mut().push(ctl);
    }

    /// Static text shown as is (safe for names and paths containing '&').
    pub fn label(&self, text: &str, rect: (i32, i32, i32, i32)) -> HWND {
        self.add("STATIC", text, SS_LEFT | SS_NOPREFIX, 0, rect)
    }

    /// A label with a keyboard mnemonic ("&Name:") for the control created after it.
    pub fn caption(&self, text: &str, rect: (i32, i32, i32, i32)) -> HWND {
        self.add("STATIC", text, SS_LEFT, 0, rect)
    }

    pub fn button(&self, text: &str, id: u16, rect: (i32, i32, i32, i32)) -> HWND {
        self.add("BUTTON", text, WS_TABSTOP | BS_PUSHBUTTON as u32, id, rect)
    }

    pub fn checkbox(&self, text: &str, id: u16, checked: bool, rect: (i32, i32, i32, i32)) -> HWND {
        let h = self.add(
            "BUTTON",
            text,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            id,
            rect,
        );
        set_checked(h, checked);
        h
    }

    pub fn edit(&self, text: &str, id: u16, rect: (i32, i32, i32, i32)) -> HWND {
        self.add_ex(
            "EDIT",
            text,
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            id,
            rect,
        )
    }

    /// Read-only text of several lines for the user to read with the arrow
    /// keys, such as release notes. Unlike an edit field, it does not select
    /// its whole text when Tab or its access key moves the focus to it (the
    /// caret would then be at the end, and reading would start there); Tab,
    /// Shift+Tab, Enter and Escape act on the window as everywhere else.
    /// Lines may end in `\n`; a new text needs [`super::crlf`] as well.
    pub fn read_only_text(&self, text: &str, id: u16, rect: (i32, i32, i32, i32)) -> HWND {
        let h = self.add_ex(
            "EDIT",
            &super::crlf(text),
            WS_TABSTOP | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
            WS_EX_CLIENTEDGE,
            id,
            rect,
        );
        unsafe { SetWindowSubclass(h, Some(read_only_text_proc), 0, 0) };
        h
    }

    /// Screen readers present the window as a dialog: when it is activated,
    /// they read its texts before the focused control (see `accessibility`).
    pub fn set_dialog_role(&self) {
        if accessibility::set_dialog_role(self.hwnd) {
            self.annotated.borrow_mut().push(self.hwnd);
        }
    }

    /// Gives a control an accessible name of its own. Windows names a control
    /// without a label of its own (a progress bar) after the label before
    /// it, and screen readers that read a dialog's texts skip a label whose
    /// text is the name of the next control, as they take it for its label.
    pub fn set_accessible_name(&self, ctl: HWND, name: &str) {
        if accessibility::set_name(ctl, name) {
            self.annotated.borrow_mut().push(ctl);
        }
    }

    /// Makes a label a live region, whose new text screen readers read after
    /// `accessibility::announce`.
    pub fn live_region(&self, ctl: HWND) {
        if accessibility::set_live_region(ctl) {
            self.annotated.borrow_mut().push(ctl);
        }
    }

    /// A drop-down list (not editable) showing `items` with `selected` chosen.
    pub fn combobox(
        &self,
        items: &[&str],
        selected: usize,
        id: u16,
        rect: (i32, i32, i32, i32),
    ) -> HWND {
        // The height includes the opened list.
        let h = self.add(
            "COMBOBOX",
            "",
            WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            id,
            (rect.0, rect.1, rect.2, rect.3 + 24 * items.len() as i32),
        );
        for item in items {
            super::send(h, CB_ADDSTRING, 0, wide(item).as_ptr() as isize);
        }
        super::send(h, CB_SETCURSEL, selected, 0);
        h
    }

    /// Width of `text` (without the access key marker) in 96-DPI units, as
    /// drawn in this window's font.
    pub fn text_width(&self, text: &str) -> i32 {
        let mut shown = String::new();
        let mut chars = super::without_shortcut(text).chars();
        while let Some(c) = chars.next() {
            // "&x" shows as "x", "&&" as "&".
            match c {
                '&' => shown.extend(chars.next()),
                c => shown.push(c),
            }
        }
        let buf: Vec<u16> = shown.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        unsafe {
            let dc = GetDC(self.hwnd);
            let old = SelectObject(dc, self.font.get());
            GetTextExtentPoint32W(dc, buf.as_ptr(), buf.len() as i32, &mut size);
            SelectObject(dc, old);
            ReleaseDC(self.hwnd, dc);
        }
        self.dip(size.cx)
    }

    /// Height in 96-DPI units of a [`label`](Self::label) `width` units wide
    /// showing `text`, wrapped at words as the label wraps it. The font
    /// follows the Windows text size setting, which DPI scaling alone does
    /// not, so a label of several lines needs this to show all of them.
    pub fn text_height(&self, text: &str, width: i32) -> i32 {
        let buf: Vec<u16> = text.encode_utf16().collect();
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: self.px(width),
            bottom: 0,
        };
        unsafe {
            let dc = GetDC(self.hwnd);
            let old = SelectObject(dc, self.font.get());
            DrawTextW(
                dc,
                buf.as_ptr(),
                buf.len() as i32,
                &mut rc,
                DT_CALCRECT | DT_WORDBREAK | DT_EXPANDTABS | DT_NOPREFIX,
            );
            SelectObject(dc, old);
            ReleaseDC(self.hwnd, dc);
        }
        // Rounded up, so that converting back to pixels loses no line.
        (rc.bottom.max(0) as u32 * 96).div_ceil(self.dpi.get()) as i32
    }

    /// Width for a push button labelled `text`: the text plus padding, at least `min`.
    pub fn button_width(&self, text: &str, min: i32) -> i32 {
        (self.text_width(text) + 24).max(min)
    }

    /// Moves an existing control to a new 96-DPI rectangle.
    pub fn place(&self, ctl: HWND, rect: (i32, i32, i32, i32)) {
        if let Some(c) = self.ctls.borrow_mut().iter_mut().find(|c| c.hwnd == ctl) {
            c.rect = rect;
        }
        unsafe {
            SetWindowPos(
                ctl,
                std::ptr::null_mut(),
                self.px(rect.0),
                self.px(rect.1),
                self.px(rect.2),
                self.px(rect.3),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    fn relayout(&self) {
        for c in self.ctls.borrow().iter() {
            unsafe {
                SetWindowPos(
                    c.hwnd,
                    std::ptr::null_mut(),
                    self.px(c.rect.0),
                    self.px(c.rect.1),
                    self.px(c.rect.2),
                    self.px(c.rect.3),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Resizes the window so that its client area has the given 96-DPI size.
    pub fn set_client_size(&self, cx: i32, cy: i32) {
        unsafe {
            let style = GetWindowLongW(self.hwnd, GWL_STYLE) as u32;
            let ex = GetWindowLongW(self.hwnd, GWL_EXSTYLE) as u32;
            let has_menu = !GetMenu(self.hwnd).is_null();
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: self.px(cx),
                bottom: self.px(cy),
            };
            match dpi_api().adjust_window_rect_ex_for_dpi {
                Some(f) => {
                    f(&mut rc, style, has_menu as i32, ex, self.dpi.get());
                }
                None => {
                    AdjustWindowRectEx(&mut rc, style, has_menu as i32, ex);
                }
            }
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Centers the window on `other` (or on the work area of its monitor).
    pub fn center(&self, other: HWND) {
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetWindowRect(self.hwnd, &mut rc);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let mut target: RECT = std::mem::zeroed();
            let visible = !other.is_null() && IsWindowVisible(other) != 0 && IsIconic(other) == 0;
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = size_of::<MONITORINFO>() as u32;
            let mon = MonitorFromWindow(
                if visible { other } else { self.hwnd },
                MONITOR_DEFAULTTONEAREST,
            );
            GetMonitorInfoW(mon, &mut mi);
            if visible {
                GetWindowRect(other, &mut target);
            } else {
                target = mi.rcWork;
            }
            let x = (target.left + (target.right - target.left - w) / 2)
                .clamp(mi.rcWork.left, (mi.rcWork.right - w).max(mi.rcWork.left));
            let y = (target.top + (target.bottom - target.top - h) / 2)
                .clamp(mi.rcWork.top, (mi.rcWork.bottom - h).max(mi.rcWork.top));
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Closes the window (re-enabling the owner of a modal window first, so
    /// that activation returns to it).
    pub fn close(&self) {
        unsafe {
            let owner = self.modal_owner.replace(std::ptr::null_mut());
            if !owner.is_null() {
                EnableWindow(owner, 1);
                SetActiveWindow(owner);
            }
            DestroyWindow(self.hwnd);
        }
    }

    /// Remembers which control has the focus (when the window is deactivated).
    fn save_focus(&self) {
        let focus = unsafe { GetFocus() };
        if !focus.is_null() && unsafe { IsChild(self.hwnd, focus) } != 0 {
            self.last_focus.set(focus);
        }
    }

    /// Gives the focus back to the remembered control, or to the first one.
    fn restore_focus(&self) {
        let last = self.last_focus.get();
        let usable = !last.is_null()
            && unsafe {
                IsChild(self.hwnd, last) != 0
                    && IsWindowVisible(last) != 0
                    && IsWindowEnabled(last) != 0
            };
        let target = if usable {
            last
        } else {
            unsafe { GetNextDlgTabItem(self.hwnd, std::ptr::null_mut(), 0) }
        };
        if !target.is_null() {
            unsafe { SetFocus(target) };
        }
    }

    pub fn client_size_dip(&self) -> (i32, i32) {
        let mut rc: RECT = unsafe { std::mem::zeroed() };
        unsafe { GetClientRect(self.hwnd, &mut rc) };
        (self.dip(rc.right), self.dip(rc.bottom))
    }
}

pub fn send_font(hwnd: HWND, font: HFONT) {
    super::send(hwnd, WM_SETFONT, font as usize, 1);
}

pub fn is_checked(hwnd: HWND) -> bool {
    super::send(hwnd, BM_GETCHECK, 0, 0) == 1
}

pub fn set_checked(hwnd: HWND, checked: bool) {
    super::send(hwnd, BM_SETCHECK, checked as usize, 0);
}

/// The selected index of a combobox, if any.
pub fn combo_selection(hwnd: HWND) -> Option<usize> {
    let i = super::send(hwnd, CB_GETCURSEL, 0, 0);
    (i >= 0).then_some(i as usize)
}

pub fn enable(hwnd: HWND, enabled: bool) {
    unsafe { EnableWindow(hwnd, enabled as i32) };
}

pub fn show(hwnd: HWND, visible: bool) {
    unsafe { ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE }) };
}

/// Moves the focus to a control. It also becomes the control that gets the
/// focus whenever its window is activated, until the user moves on.
pub fn focus(hwnd: HWND) {
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    with_window(root, |w| w.last_focus.set(hwnd));
    // A hidden window gets it when it is shown and activated.
    if unsafe { IsWindowVisible(root) } != 0 {
        unsafe { SetFocus(hwnd) };
    }
}

pub fn is_minimized(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd) != 0 }
}

/// Brings a (possibly hidden or minimized) window to the front.
pub fn bring_to_front(hwnd: HWND) {
    let show = if is_minimized(hwnd) {
        SW_RESTORE
    } else {
        SW_SHOW
    };
    unsafe {
        ShowWindow(hwnd, show);
        SetForegroundWindow(hwnd);
    }
}

/// A multiline edit control asks `IsDialogMessageW` for every key, Tab
/// included, and outside real dialog boxes keeps the focus on Tab; it also
/// asks to have its whole text selected when it gets the focus. Text that is
/// only read wants neither.
unsafe extern "system" fn read_only_text_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(read_only_text_proc), 0);
        }
        let r = DefSubclassProc(hwnd, msg, wp, lp);
        if msg == WM_GETDLGCODE {
            return r & !((DLGC_WANTALLKEYS | DLGC_HASSETSEL) as LRESULT);
        }
        r
    }
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lp as *const CREATESTRUCTW);
            let state = cs.lpCreateParams as *mut State;
            (*state).win.hwnd = hwnd;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as _);
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as usize as *mut State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        let state = &*ptr;
        state.depth.set(state.depth.get() + 1);
        let r = dispatch(state, hwnd, msg, wp, lp);
        leave(ptr);
        r
    }
}

unsafe fn dispatch(s: &State, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let w = &s.win;
    // Messages for the handler; if it is busy (re-entrant call) fall back to defaults.
    macro_rules! handler {
        () => {
            s.handler.try_borrow_mut()
        };
    }
    unsafe {
        if let Some(r) = theme::handle_menubar(hwnd, msg, wp, lp) {
            return r;
        }
        // Network events are handled outside of any handler borrow so they can
        // update every window (including this one).
        if msg == crate::app::WM_APP_NET {
            crate::app::process_events();
            return 0;
        }
        if let Some(urgent) = s.urgent
            && let Some(r) = urgent(w, msg, wp, lp)
        {
            return r;
        }
        if let Ok(mut h) = handler!()
            && let Some(r) = h.on_message(w, msg, wp, lp)
        {
            return r;
        }
        match msg {
            WM_ERASEBKGND => {
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                FillRect(wp as HDC, &rc, theme::window_brush());
                1
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX
            | WM_CTLCOLORDLG => theme::ctl_color(msg, wp as HDC, lp as HWND)
                .unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp)),
            WM_SETTINGCHANGE => {
                if lp != 0 && super::from_wide_ptr(lp as *const u16) == "ImmersiveColorSet" {
                    theme::refresh();
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_THEMECHANGED => {
                theme::refresh();
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCPAINT | WM_NCACTIVATE => {
                let r = DefWindowProcW(hwnd, msg, wp, lp);
                theme::paint_menubar_line(hwnd);
                r
            }
            WM_DPICHANGED => {
                w.dpi.set(hiword(wp) as u32);
                w.update_fonts();
                let rc = &*(lp as *const RECT);
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                w.relayout();
                let (cx, cy) = w.client_size_dip();
                if let Ok(mut h) = handler!() {
                    h.on_size(w, cx, cy);
                }
                0
            }
            WM_SIZE => {
                let (cx, cy) = (
                    w.dip(loword(lp as usize) as i32),
                    w.dip(hiword(lp as usize) as i32),
                );
                if let Ok(mut h) = handler!() {
                    h.on_size(w, cx, cy);
                }
                0
            }
            // Like dialog boxes: the focused control is remembered when the
            // window loses activation (e.g. Alt+Tab) and restored afterwards.
            WM_ACTIVATE => {
                if loword(wp) as u32 == WA_INACTIVE {
                    w.save_focus();
                } else if hiword(wp) == 0 {
                    w.restore_focus();
                    return 0;
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SETFOCUS => {
                w.restore_focus();
                0
            }
            DM_GETDEFID => match w.default_id.get() {
                0 => 0,
                id => (DC_HASDEFID << 16) | id as isize,
            },
            WM_COMMAND => {
                let (id, code) = (loword(wp), hiword(wp));
                let handled = match handler!() {
                    Ok(mut h) => h.on_command(w, id, code),
                    Err(_) => true,
                };
                // Escape (IDCANCEL from IsDialogMessageW) closes dialog-like
                // windows. The main window ignores it, like other programs'
                // main windows; closing it would hide it to the notification area.
                if !handled && id == IDCANCEL as u16 && !w.is_main {
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                0
            }
            WM_NOTIFY => {
                let hdr = &*(lp as *const NMHDR);
                match handler!() {
                    Ok(mut h) => h.on_notify(w, hdr, lp).unwrap_or(0),
                    Err(_) => 0,
                }
            }
            WM_CLOSE => {
                if let Ok(mut h) = handler!() {
                    h.on_close(w);
                }
                0
            }
            WM_DESTROY => {
                for h in w.annotated.take() {
                    accessibility::clear(h);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                TOP_LEVEL.with(|t| t.borrow_mut().retain(|&x| x != hwnd));
                s.dead.set(true);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
