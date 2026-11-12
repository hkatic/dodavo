//! Notification area (system tray) icon. While transfers run, the icon
//! carries a badge with their number, and its tooltip (what a screen reader
//! reads on the icon) says what is going on.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
};

use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{
        ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
        CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET,
        DEFAULT_PITCH, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC,
        DeleteObject, DrawTextW, FF_DONTCARE, FW_BOLD, GdiFlush, GetDIBits, HBITMAP,
        OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_NOSOUND, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            CreateIconIndirect, GetIconInfo, GetSystemMetrics, HICON, ICONINFO, IMAGE_ICON,
            LR_DEFAULTCOLOR, LoadImageW, SM_CXSMICON, SM_CYSMICON, WM_APP,
        },
    },
};

use super::{APP_NAME, wide};

pub const WM_APP_TRAY: u32 = WM_APP + 2;
const TRAY_ID: u32 = 1;
/// The badge colour (red, like unread counts elsewhere in Windows), as (B, G, R).
const BADGE_COLOR: (f32, f32, f32) = (0x38 as f32, 0x34 as f32, 0xD1 as f32);

/// What selecting a notification does.
pub enum BalloonAction {
    /// Shows a file or folder in File Explorer.
    Reveal(PathBuf),
    /// Brings a transfer's progress window to the front, or the main window
    /// once that has closed. Windows are named by what they are for, never by
    /// handle: the handle of a closed window may come back for a new,
    /// unrelated window.
    Transfer(u64),
    /// Brings the Update Available window to the front, or the main window
    /// once that has closed.
    Update,
    /// Brings the Send with Dodavo window to the front, or the main window
    /// once that has closed.
    SendTo,
    /// Opens the chat with the person who has the Contact ID `peer`, and
    /// is shown as `name` unless they are a recipient.
    Chat { peer: String, name: String },
}

thread_local! {
    /// The program icon at the small icon size, loaded once.
    static BASE_ICON: Cell<HICON> = const { Cell::new(std::ptr::null_mut()) };
    /// Icons with a count badge, by label; they live for the process.
    static BADGES: RefCell<HashMap<String, HICON>> = RefCell::new(HashMap::new());
    /// The current state (running transfers, tooltip), re-applied when the
    /// taskbar is recreated.
    static STATUS: RefCell<(usize, String)> = RefCell::new((0, APP_NAME.to_string()));
}

fn copy_into(dst: &mut [u16], text: &str) {
    let mut i = 0;
    for c in text.encode_utf16() {
        if i + 1 >= dst.len() {
            break;
        }
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ID;
    nid
}

fn base_icon() -> HICON {
    let icon = BASE_ICON.get();
    if !icon.is_null() {
        return icon;
    }
    let icon = unsafe {
        LoadImageW(
            GetModuleHandleW(std::ptr::null()),
            1 as _,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        ) as HICON
    };
    BASE_ICON.set(icon);
    icon
}

/// The icon for a number of running transfers.
fn icon_for(count: usize) -> HICON {
    if count == 0 {
        return base_icon();
    }
    let label = badge_label(count);
    if let Some(&icon) = BADGES.with(|b| b.borrow().get(&label).copied()).as_ref() {
        return icon;
    }
    let icon = badge::make(base_icon(), &label);
    let icon = if icon.is_null() { base_icon() } else { icon };
    BADGES.with(|b| b.borrow_mut().insert(label, icon));
    icon
}

/// The text in the badge: the count, or "9+" when it does not fit.
fn badge_label(count: usize) -> String {
    if count > 9 {
        "9+".to_string()
    } else {
        count.to_string()
    }
}

pub fn add(hwnd: HWND) {
    let (count, tip) = STATUS.with(|s| s.borrow().clone());
    let mut nid = base(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.hIcon = icon_for(count);
    // The struct is packed on 32-bit Windows: fill the arrays through copies.
    let mut tip_buf = nid.szTip;
    copy_into(&mut tip_buf, &tip);
    nid.szTip = tip_buf;
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
}

pub fn remove(hwnd: HWND) {
    let nid = base(hwnd);
    unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// Shows the number of running transfers on the icon and `tip` as its tooltip.
pub fn set_status(hwnd: HWND, count: usize, tip: &str) {
    STATUS.with(|s| *s.borrow_mut() = (count, tip.to_string()));
    if hwnd.is_null() {
        return;
    }
    let mut nid = base(hwnd);
    nid.uFlags = NIF_ICON | NIF_TIP;
    nid.hIcon = icon_for(count);
    let mut tip_buf = nid.szTip;
    copy_into(&mut tip_buf, tip);
    nid.szTip = tip_buf;
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

/// Shows a notification. Selecting it performs `action` (or shows the main
/// window). Not to be called inside `app::with`.
pub fn balloon(hwnd: HWND, title: &str, text: &str, action: Option<BalloonAction>) {
    crate::app::with(|a| a.balloon_action = action);
    let mut nid = base(hwnd);
    nid.uFlags = NIF_INFO;
    // The event has just had its own sound: Windows' is left out.
    nid.dwInfoFlags = if super::sound::just_played() {
        NIIF_INFO | NIIF_NOSOUND
    } else {
        NIIF_INFO
    };
    let mut info_title = nid.szInfoTitle;
    copy_into(&mut info_title, title);
    nid.szInfoTitle = info_title;
    let mut info = nid.szInfo;
    copy_into(&mut info, text);
    nid.szInfo = info;
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

/// Drawing the count badge onto the program icon. The pixels are composed in
/// software (GDI drawing would not keep the alpha channel); GDI only
/// provides the icon's pixels, the glyphs and the final icon.
mod badge {
    use super::*;

    /// Pixels of a bitmap as top-down 32-bit BGRA.
    unsafe fn pixels(bitmap: HBITMAP, cx: i32, cy: i32) -> Option<Vec<u32>> {
        let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: cx,
            biHeight: -cy,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let mut buf = vec![0u32; (cx * cy) as usize];
        let hdc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        let lines = unsafe {
            GetDIBits(
                hdc,
                bitmap,
                0,
                cy as u32,
                buf.as_mut_ptr().cast(),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };
        unsafe { DeleteDC(hdc) };
        (lines == cy).then_some(buf)
    }

    /// The glyph coverage (0–255) of `label`, rendered white on black into a
    /// `size` × `size` square.
    unsafe fn glyphs(label: &str, size: i32) -> Option<Vec<u8>> {
        let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let hdc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = unsafe {
            CreateDIBSection(
                hdc,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if dib.is_null() || bits.is_null() {
            unsafe { DeleteDC(hdc) };
            return None;
        }
        // One digit fills most of the badge; "9+" needs a smaller face.
        let height = if label.chars().count() > 1 {
            size * 7 / 10
        } else {
            size * 17 / 20
        };
        let face = wide("Segoe UI");
        let font = unsafe {
            CreateFontW(
                -height.max(5),
                0,
                0,
                0,
                FW_BOLD as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            )
        };
        let text = wide(label);
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        let coverage = unsafe {
            let old_bitmap = SelectObject(hdc, dib as _);
            let old_font = SelectObject(hdc, font as _);
            SetBkMode(hdc, TRANSPARENT as i32);
            SetTextColor(hdc, 0x00FF_FFFF);
            DrawTextW(
                hdc,
                text.as_ptr(),
                -1,
                &mut rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            GdiFlush();
            let px = std::slice::from_raw_parts(bits as *const u32, (size * size) as usize);
            let coverage: Vec<u8> = px.iter().map(|p| (p & 0xFF) as u8).collect();
            SelectObject(hdc, old_font);
            SelectObject(hdc, old_bitmap);
            DeleteObject(font as _);
            DeleteObject(dib as _);
            DeleteDC(hdc);
            coverage
        };
        Some(coverage)
    }

    /// Blends `(b, g, r)` with coverage `cov` over a straight-alpha BGRA pixel.
    fn blend(dst: u32, (b, g, r): (f32, f32, f32), cov: f32) -> u32 {
        let da = ((dst >> 24) & 0xFF) as f32 / 255.0;
        let channel = |shift: u32, src: f32| -> f32 {
            let d = ((dst >> shift) & 0xFF) as f32;
            let out_a = cov + da * (1.0 - cov);
            if out_a <= 0.0 {
                0.0
            } else {
                (src * cov + d * da * (1.0 - cov)) / out_a
            }
        };
        let out_a = cov + da * (1.0 - cov);
        let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u32;
        (to_u8(out_a * 255.0) << 24)
            | (to_u8(channel(16, r)) << 16)
            | (to_u8(channel(8, g)) << 8)
            | to_u8(channel(0, b))
    }

    /// The program icon with `label` in a round badge at the bottom right,
    /// or null if anything fails.
    pub fn make(base: HICON, label: &str) -> HICON {
        let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
        if base.is_null() || cx < 8 || cy < 8 {
            return std::ptr::null_mut();
        }
        let mut info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(base, &mut info) } == 0 {
            return std::ptr::null_mut();
        }
        let color = unsafe { pixels(info.hbmColor, cx, cy) };
        let mask = unsafe { pixels(info.hbmMask, cx, cy) };
        unsafe {
            DeleteObject(info.hbmColor as _);
            DeleteObject(info.hbmMask as _);
        }
        let (Some(mut px), Some(mask)) = (color, mask) else {
            return std::ptr::null_mut();
        };
        // An icon without an alpha channel is opaque where its mask is black.
        if px.iter().all(|p| p >> 24 == 0) {
            for (p, m) in px.iter_mut().zip(&mask) {
                if m & 0x00FF_FFFF == 0 {
                    *p |= 0xFF00_0000;
                }
            }
        }

        let size = ((cy as f32) * 0.62).round().max(7.0) as i32;
        let Some(glyphs) = (unsafe { glyphs(label, size) }) else {
            return std::ptr::null_mut();
        };
        let (x0, y0) = (cx - size, cy - size);
        let radius = size as f32 / 2.0;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
                let cov = (radius - (fx * fx + fy * fy).sqrt() + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let i = ((y0 + y) * cx + x0 + x) as usize;
                px[i] = blend(px[i], BADGE_COLOR, cov);
                let text = glyphs[(y * size + x) as usize] as f32 / 255.0 * cov;
                if text > 0.0 {
                    px[i] = blend(px[i], (255.0, 255.0, 255.0), text);
                }
            }
        }

        unsafe {
            let color = CreateBitmap(cx, cy, 1, 32, px.as_ptr().cast());
            let empty = vec![0u8; ((cx + 15) / 16 * 2 * cy) as usize];
            let mask = CreateBitmap(cx, cy, 1, 1, empty.as_ptr().cast());
            let info = ICONINFO {
                fIcon: 1,
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            let icon = CreateIconIndirect(&info);
            DeleteObject(color as _);
            DeleteObject(mask as _);
            icon
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_label_caps_at_nine() {
        assert_eq!(badge_label(1), "1");
        assert_eq!(badge_label(9), "9");
        assert_eq!(badge_label(10), "9+");
        assert_eq!(badge_label(42), "9+");
    }

    #[test]
    fn badge_icons_are_created() {
        // The test binary has no icon resource, so use a stock one.
        use windows_sys::Win32::UI::WindowsAndMessaging::{IDI_APPLICATION, LoadIconW};
        let base = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
        assert!(!base.is_null());
        for label in ["1", "7", "9+"] {
            assert!(!badge::make(base, label).is_null(), "badge {label}");
        }
    }
}
