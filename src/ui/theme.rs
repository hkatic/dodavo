//! Light/dark mode support for classic Win32 windows.
//!
//! Windows has no official dark mode for Win32 controls, so this uses the
//! same techniques as Notepad++, Windows Terminal's settings, etc.:
//! - the DWM attribute for a dark title bar,
//! - the (undocumented but stable) uxtheme ordinals for dark context menus and
//!   scroll bars, and the `DarkMode_*` visual style classes for controls,
//! - WM_CTLCOLOR* for static text, edits and dialogs,
//! - owner-drawing the menu bar through the undocumented UAH messages.
//!
//! Everything degrades gracefully to the light look if any piece is missing.

use std::cell::Cell;

use windows_sys::{
    Win32::{
        Foundation::{COLORREF, HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Dwm::DwmSetWindowAttribute,
            Gdi::{
                COLOR_BTNFACE, COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateSolidBrush,
                DT_CENTER, DT_HIDEPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW,
                FillRect, GetSysColor, GetSysColorBrush, GetWindowDC, HBRUSH, HDC, MapWindowPoints,
                OffsetRect, RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow,
                ReleaseDC, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
            },
        },
        System::{
            LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        },
        UI::{
            Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
            Controls::{
                CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW,
                DRAWITEMSTRUCT, LVM_GETHEADER, LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR,
                LVM_SETTEXTCOLOR, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, ODS_DISABLED, ODS_GRAYED,
                ODS_HOTLIGHT, ODS_INACTIVE, ODS_NOACCEL, ODS_SELECTED, PBM_SETBARCOLOR,
                PBM_SETBKCOLOR, SetWindowTheme,
            },
            Input::KeyboardAndMouse::IsWindowEnabled,
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    core::PCWSTR,
};

use super::{send, wide};

const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const WM_UAHDRAWMENU: u32 = 0x0091;
const WM_UAHDRAWMENUITEM: u32 = 0x0092;
const CLR_DEFAULT: u32 = 0xFF00_0000;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const DARK_BG: COLORREF = rgb(32, 32, 32);
const DARK_INPUT_BG: COLORREF = rgb(43, 43, 43);
const DARK_TEXT: COLORREF = rgb(240, 240, 240);
const DARK_GRAY_TEXT: COLORREF = rgb(140, 140, 140);
const DARK_MENU_HOT: COLORREF = rgb(65, 65, 65);
const DARK_PROGRESS: COLORREF = rgb(76, 194, 255);

#[repr(C)]
struct UahMenu {
    hmenu: HMENU,
    hdc: HDC,
    flags: u32,
}

#[repr(C)]
struct UahMenuItem {
    position: i32,
    metrics: [u32; 8],
    popup_metrics: [u32; 5],
}

#[repr(C)]
struct UahDrawMenuItem {
    dis: DRAWITEMSTRUCT,
    um: UahMenu,
    umi: UahMenuItem,
}

type FnSetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type FnAllowDarkModeForWindow = unsafe extern "system" fn(HWND, bool) -> bool;
type FnVoid = unsafe extern "system" fn();

#[derive(Clone, Copy)]
struct Api {
    set_preferred_app_mode: Option<FnSetPreferredAppMode>,
    allow_dark_mode_for_window: Option<FnAllowDarkModeForWindow>,
    refresh_policy: Option<FnVoid>,
    flush_menu_themes: Option<FnVoid>,
}

#[derive(Clone, Copy)]
struct Brushes {
    window: HBRUSH,
    input: HBRUSH,
    menu_hot: HBRUSH,
}

thread_local! {
    static DARK: Cell<bool> = const { Cell::new(false) };
    static API: Cell<Option<Api>> = const { Cell::new(None) };
    static BRUSHES: Cell<Option<Brushes>> = const { Cell::new(None) };
}

fn api() -> Api {
    if let Some(api) = API.get() {
        return api;
    }
    let api = unsafe {
        let module: HMODULE = LoadLibraryExW(
            wide("uxtheme.dll").as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if module.is_null() {
            Api {
                set_preferred_app_mode: None,
                allow_dark_mode_for_window: None,
                refresh_policy: None,
                flush_menu_themes: None,
            }
        } else {
            let ord = |n: usize| GetProcAddress(module, n as *const u8);
            Api {
                set_preferred_app_mode: ord(135).map(|f| std::mem::transmute(f)),
                allow_dark_mode_for_window: ord(133).map(|f| std::mem::transmute(f)),
                refresh_policy: ord(104).map(|f| std::mem::transmute(f)),
                flush_menu_themes: ord(136).map(|f| std::mem::transmute(f)),
            }
            // uxtheme stays loaded for the lifetime of the process.
        }
    };
    API.set(Some(api));
    api
}

/// Reads the "Choose your app mode" setting.
fn system_prefers_dark() -> bool {
    // Developer override for testing without changing the system setting.
    match std::env::var("DODAVO_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    unsafe {
        let mut hc: HIGHCONTRASTW = std::mem::zeroed();
        hc.cbSize = size_of::<HIGHCONTRASTW>() as u32;
        if SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, &mut hc as *mut _ as _, 0) != 0
            && hc.dwFlags & HCF_HIGHCONTRASTON != 0
        {
            return false;
        }
        let mut value: u32 = 1;
        let mut size = size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize").as_ptr(),
            wide("AppsUseLightTheme").as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut value as *mut u32 as _,
            &mut size,
        );
        status == 0 && value == 0
    }
}

pub fn is_dark() -> bool {
    DARK.get()
}

fn set_mode(dark: bool) {
    DARK.set(dark);
    if let Some(old) = BRUSHES.take() {
        unsafe {
            DeleteObject(old.window);
            DeleteObject(old.input);
            DeleteObject(old.menu_hot);
        }
    }
    unsafe {
        BRUSHES.set(Some(if dark {
            Brushes {
                window: CreateSolidBrush(DARK_BG),
                input: CreateSolidBrush(DARK_INPUT_BG),
                menu_hot: CreateSolidBrush(DARK_MENU_HOT),
            }
        } else {
            Brushes {
                window: CreateSolidBrush(GetSysColor(COLOR_BTNFACE)),
                input: CreateSolidBrush(GetSysColor(COLOR_WINDOW)),
                menu_hot: CreateSolidBrush(GetSysColor(COLOR_HIGHLIGHT)),
            }
        }));
    }
    let api = api();
    unsafe {
        if let Some(f) = api.set_preferred_app_mode {
            // 2 = ForceDark, 3 = ForceLight
            f(if dark { 2 } else { 3 });
        }
        if let Some(f) = api.refresh_policy {
            f();
        }
        if let Some(f) = api.flush_menu_themes {
            f();
        }
    }
}

/// Detects the current mode. Call once at startup before creating windows.
pub fn init() {
    set_mode(system_prefers_dark());
}

/// Re-reads the system setting and re-themes all windows if it changed.
pub fn refresh() {
    let dark = system_prefers_dark();
    if dark == is_dark() {
        return;
    }
    set_mode(dark);
    for hwnd in super::window::all_windows() {
        apply_to_window(hwnd);
    }
}

fn brushes() -> Brushes {
    BRUSHES.get().unwrap_or_else(|| {
        set_mode(false);
        BRUSHES.get().unwrap()
    })
}

pub fn window_brush() -> HBRUSH {
    if is_dark() {
        brushes().window
    } else {
        unsafe { GetSysColorBrush(COLOR_BTNFACE) }
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn set_theme(hwnd: HWND, app: Option<&str>) {
    let app = app.map(wide);
    let p: PCWSTR = app.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    unsafe { SetWindowTheme(hwnd, p, std::ptr::null()) };
}

fn allow_dark(hwnd: HWND, dark: bool) {
    if let Some(f) = api().allow_dark_mode_for_window {
        unsafe { f(hwnd, dark) };
    }
}

/// Applies the current mode to a top-level window and all of its controls.
pub fn apply_to_window(hwnd: HWND) {
    let dark = is_dark();
    unsafe {
        allow_dark(hwnd, dark);
        let value: i32 = dark as i32;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &value as *const i32 as _,
            4,
        );
        EnumChildWindows(hwnd, Some(theme_child), 0);
        DrawMenuBar(hwnd);
        RedrawWindow(
            hwnd,
            std::ptr::null(),
            std::ptr::null_mut(),
            RDW_ERASE | RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN,
        );
    }
}

unsafe extern "system" fn theme_child(hwnd: HWND, _: LPARAM) -> i32 {
    let dark = is_dark();
    allow_dark(hwnd, dark);
    let class = class_name(hwnd).to_ascii_lowercase();
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    match class.as_str() {
        "button" => {
            let kind = style & BS_TYPEMASK as u32;
            let is_push = kind == BS_PUSHBUTTON as u32 || kind == BS_DEFPUSHBUTTON as u32;
            if is_push {
                set_theme(
                    hwnd,
                    Some(if dark {
                        "DarkMode_Explorer"
                    } else {
                        "Explorer"
                    }),
                );
            } else if dark {
                // Themed check boxes ignore WM_CTLCOLORSTATIC text colors; the
                // classic look honours them and stays readable on dark backgrounds.
                set_theme(hwnd, Some(""));
            } else {
                set_theme(hwnd, None);
            }
        }
        "edit" | "combobox" => set_theme(hwnd, if dark { Some("DarkMode_CFD") } else { None }),
        "syslistview32" => {
            set_theme(
                hwnd,
                Some(if dark {
                    "DarkMode_Explorer"
                } else {
                    "Explorer"
                }),
            );
            let (bg, fg) = if dark {
                (DARK_INPUT_BG, DARK_TEXT)
            } else {
                unsafe { (GetSysColor(COLOR_WINDOW), GetSysColor(COLOR_WINDOWTEXT)) }
            };
            send(hwnd, LVM_SETBKCOLOR, 0, bg as isize);
            send(hwnd, LVM_SETTEXTBKCOLOR, 0, bg as isize);
            send(hwnd, LVM_SETTEXTCOLOR, 0, fg as isize);
            let header = send(hwnd, LVM_GETHEADER, 0, 0) as HWND;
            if !header.is_null() {
                allow_dark(header, dark);
                set_theme(
                    header,
                    Some(if dark {
                        "DarkMode_ItemsView"
                    } else {
                        "ItemsView"
                    }),
                );
            }
            unsafe { SetWindowSubclass(hwnd, Some(listview_subclass), 1, 0) };
        }
        "msctls_progress32" => {
            if dark {
                set_theme(hwnd, Some(""));
                send(hwnd, PBM_SETBKCOLOR, 0, DARK_INPUT_BG as isize);
                send(hwnd, PBM_SETBARCOLOR, 0, DARK_PROGRESS as isize);
            } else {
                set_theme(hwnd, None);
                send(hwnd, PBM_SETBKCOLOR, 0, CLR_DEFAULT as isize);
                send(hwnd, PBM_SETBARCOLOR, 0, CLR_DEFAULT as isize);
            }
        }
        _ => {}
    }
    send(hwnd, WM_THEMECHANGED, 0, 0);
    1
}

/// Colors the list view header text in dark mode.
unsafe extern "system" fn listview_subclass(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NOTIFY && is_dark() {
            let hdr = &*(lp as *const NMHDR);
            if hdr.code == NM_CUSTOMDRAW {
                let cd = &*(lp as *const NMCUSTOMDRAW);
                if cd.dwDrawStage == CDDS_PREPAINT {
                    return CDRF_NOTIFYITEMDRAW as isize;
                }
                if cd.dwDrawStage == CDDS_ITEMPREPAINT {
                    SetTextColor(cd.hdc, DARK_TEXT);
                    return CDRF_DODEFAULT as isize;
                }
            }
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

/// Handles WM_CTLCOLOR* in dark mode.
pub fn ctl_color(msg: u32, hdc: HDC, ctl: HWND) -> Option<LRESULT> {
    if !is_dark() {
        return None;
    }
    let b = brushes();
    unsafe {
        let is_edit = class_name(ctl).eq_ignore_ascii_case("edit");
        let (brush, bg) = match msg {
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => (b.input, DARK_INPUT_BG),
            // Read-only edits send WM_CTLCOLORSTATIC.
            WM_CTLCOLORSTATIC if is_edit => (b.input, DARK_INPUT_BG),
            _ => (b.window, DARK_BG),
        };
        let enabled = IsWindowEnabled(ctl) != 0;
        SetTextColor(hdc, if enabled { DARK_TEXT } else { DARK_GRAY_TEXT });
        SetBkColor(hdc, bg);
        Some(brush as LRESULT)
    }
}

/// Draws the menu bar in dark mode (WM_UAHDRAWMENU / WM_UAHDRAWMENUITEM).
pub fn handle_menubar(hwnd: HWND, msg: u32, _wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !is_dark() || (msg != WM_UAHDRAWMENU && msg != WM_UAHDRAWMENUITEM) {
        return None;
    }
    let b = brushes();
    unsafe {
        if msg == WM_UAHDRAWMENU {
            let udm = &*(lp as *const UahMenu);
            let mut mbi: MENUBARINFO = std::mem::zeroed();
            mbi.cbSize = size_of::<MENUBARINFO>() as u32;
            GetMenuBarInfo(hwnd, OBJID_MENU, 0, &mut mbi);
            let mut wr: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut wr);
            let mut rc = mbi.rcBar;
            OffsetRect(&mut rc, -wr.left, -wr.top);
            rc.top -= 1;
            FillRect(udm.hdc, &rc, b.window);
            return Some(1);
        }
        let item = &*(lp as *const UahDrawMenuItem);
        let mut buf = [0u16; 256];
        let mut mii: MENUITEMINFOW = std::mem::zeroed();
        mii.cbSize = size_of::<MENUITEMINFOW>() as u32;
        mii.fMask = MIIM_STRING;
        mii.dwTypeData = buf.as_mut_ptr();
        mii.cch = (buf.len() - 1) as u32;
        GetMenuItemInfoW(item.um.hmenu, item.umi.position as u32, 1, &mut mii);
        let state = item.dis.itemState;
        let hot = state & (ODS_HOTLIGHT | ODS_SELECTED) != 0;
        let disabled = state & (ODS_INACTIVE | ODS_DISABLED | ODS_GRAYED) != 0;
        let mut flags = DT_CENTER | DT_SINGLELINE | DT_VCENTER;
        if state & ODS_NOACCEL != 0 {
            flags |= DT_HIDEPREFIX;
        }
        let mut rc = item.dis.rcItem;
        FillRect(item.um.hdc, &rc, if hot { b.menu_hot } else { b.window });
        SetBkMode(item.um.hdc, TRANSPARENT as i32);
        SetTextColor(
            item.um.hdc,
            if disabled { DARK_GRAY_TEXT } else { DARK_TEXT },
        );
        DrawTextW(item.um.hdc, buf.as_ptr(), -1, &mut rc, flags);
        Some(1)
    }
}

/// Paints over the light line Windows draws below the menu bar.
pub fn paint_menubar_line(hwnd: HWND) {
    unsafe {
        if !is_dark() || GetMenu(hwnd).is_null() {
            return;
        }
        let mut rc: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rc);
        MapWindowPoints(hwnd, std::ptr::null_mut(), &mut rc as *mut RECT as _, 2);
        let mut wr: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut wr);
        OffsetRect(&mut rc, -wr.left, -wr.top);
        let mut line = rc;
        line.bottom = line.top;
        line.top -= 1;
        let hdc = GetWindowDC(hwnd);
        FillRect(hdc, &line, brushes().window);
        ReleaseDC(hwnd, hdc);
    }
}
