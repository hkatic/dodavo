//! What plain Win32 windows cannot tell screen readers by themselves, set
//! with Dynamic Annotation (`IAccPropServices` in oleacc.dll, which every
//! supported Windows has):
//!
//! - The dialog role. Screen readers read the texts of a dialog when it is
//!   activated, as in a message box; of other windows only the title and the
//!   focused control. NVDA treats every window with this role as a dialog,
//!   whatever its window class.
//! - Live regions: after [`announce`], screen readers read the new text of a
//!   status line, which they do not do when a label just changes. NVDA
//!   reacts to the event alone; Narrator also needs the LiveSetting property,
//!   which Windows 7 doesn't know (there it has no effect).
//!
//! - Names, for a control that would otherwise be named after the label
//!   before it.
//!
//! Annotations stay until they are cleared, which `window` does before the
//! window is destroyed.

use std::mem::ManuallyDrop;

use windows::{
    Win32::{
        Foundation::HWND as WHWND,
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4},
        },
        UI::Accessibility::{
            CAccPropServices, IAccPropServices, LiveSetting_Property_GUID, PROPID_ACC_NAME,
            PROPID_ACC_ROLE, Polite, ROLE_SYSTEM_DIALOG,
        },
    },
    core::{GUID, HSTRING},
};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Accessibility::NotifyWinEvent,
        WindowsAndMessaging::{CHILDID_SELF, EVENT_OBJECT_LIVEREGIONCHANGED, OBJID_CLIENT},
    },
};

const PROPS: [GUID; 3] = [PROPID_ACC_ROLE, PROPID_ACC_NAME, LiveSetting_Property_GUID];

fn services() -> Option<IAccPropServices> {
    // COM is initialized on the UI thread (`main`).
    unsafe { CoCreateInstance(&CAccPropServices, None, CLSCTX_INPROC_SERVER) }.ok()
}

fn int_variant(value: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                Anonymous: VARIANT_0_0_0 { lVal: value },
                ..Default::default()
            }),
        },
    }
}

/// Sets a property of the window's client object; returns whether it was set.
fn set(hwnd: HWND, prop: GUID, value: i32) -> bool {
    let Some(services) = services() else {
        return false;
    };
    unsafe {
        services.SetHwndProp(
            WHWND(hwnd),
            OBJID_CLIENT as u32,
            CHILDID_SELF,
            prop,
            &int_variant(value),
        )
    }
    .is_ok()
}

/// Gives a top-level window the dialog role (see the module documentation).
pub fn set_dialog_role(hwnd: HWND) -> bool {
    set(hwnd, PROPID_ACC_ROLE, ROLE_SYSTEM_DIALOG as i32)
}

/// Makes a control (a status label) a polite live region for [`announce`].
pub fn set_live_region(ctl: HWND) -> bool {
    set(ctl, LiveSetting_Property_GUID, Polite.0)
}

/// Gives a control its own accessible name.
pub fn set_name(ctl: HWND, name: &str) -> bool {
    let Some(services) = services() else {
        return false;
    };
    unsafe {
        services.SetHwndPropStr(
            WHWND(ctl),
            OBJID_CLIENT as u32,
            CHILDID_SELF,
            PROPID_ACC_NAME,
            &HSTRING::from(name),
        )
    }
    .is_ok()
}

/// Has screen readers read the current text of a live region, without
/// interrupting what they are saying. Call it after the text was set, and
/// only for a text that stays for a moment: screen readers fetch the text
/// when they handle the event, so a text replaced right away is read twice
/// in its new form. A change of focus right afterwards interrupts it.
pub fn announce(ctl: HWND) {
    unsafe {
        NotifyWinEvent(
            EVENT_OBJECT_LIVEREGIONCHANGED,
            ctl,
            OBJID_CLIENT,
            CHILDID_SELF as i32,
        )
    };
}

/// Removes the annotations of a window or control.
pub fn clear(hwnd: HWND) {
    if let Some(services) = services() {
        let _ = unsafe {
            services.ClearHwndProps(WHWND(hwnd), OBJID_CLIENT as u32, CHILDID_SELF, &PROPS)
        };
    }
}
