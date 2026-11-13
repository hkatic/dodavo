//! Windows: handing over to Setup, as described in `update/mod.rs`.

use std::path::{Path, PathBuf};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, HANDLE, WAIT_OBJECT_0},
    System::{
        Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY,
            KEY_WOW64_64KEY, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
        },
        Threading::{GetExitCodeProcess, WaitForSingleObject},
    },
    UI::{
        Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            ShellExecuteExW,
        },
        WindowsAndMessaging::SW_SHOWNORMAL,
    },
};

use crate::platform::wide;

use super::{Error, log_path};

/// How the running program was installed, which decides how Setup must run
/// to replace it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// `%LOCALAPPDATA%\Programs\Dodavo`, registered under HKEY_CURRENT_USER.
    PerUser,
    /// Program Files, registered under HKEY_LOCAL_MACHINE; Setup needs
    /// administrator rights and asks for them (elevation prompt).
    AllUsers,
    /// Neither registration matches this program (a portable copy, a
    /// development build): Setup decides by itself.
    Unknown,
}

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{34D1793E-8FBB-46FC-BC27-09F31F071943}_is1";

/// Decides the install mode from where the program runs and where the two
/// possible registrations say Dodavo is installed.
pub fn mode_for(
    exe_dir: Option<&Path>,
    per_user: Option<&Path>,
    all_users: Option<&Path>,
) -> InstallMode {
    fn normal(p: &Path) -> String {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    }
    let Some(exe_dir) = exe_dir.map(normal) else {
        return InstallMode::Unknown;
    };
    if all_users.is_some_and(|d| normal(d) == exe_dir) {
        InstallMode::AllUsers
    } else if per_user.is_some_and(|d| normal(d) == exe_dir) {
        InstallMode::PerUser
    } else {
        InstallMode::Unknown
    }
}

/// `InstallLocation` of Dodavo's uninstall entry under `root`, if there is one.
fn uninstall_location(root: HKEY, wow_flag: u32) -> Option<PathBuf> {
    let key = wide(UNINSTALL_KEY);
    let value = wide("InstallLocation");
    let mut handle: HKEY = std::ptr::null_mut();
    let opened = unsafe { RegOpenKeyExW(root, key.as_ptr(), 0, KEY_READ | wow_flag, &mut handle) };
    if opened != ERROR_SUCCESS {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let mut kind = 0u32;
    let read = unsafe {
        RegQueryValueExW(
            handle,
            value.as_ptr(),
            std::ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    unsafe { RegCloseKey(handle) };
    if read != ERROR_SUCCESS || kind != REG_SZ {
        return None;
    }
    let text = crate::platform::from_wide_ptr(buf.as_ptr());
    (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()))
}

/// The install mode of the running program.
pub fn install_mode() -> InstallMode {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let per_user = uninstall_location(HKEY_CURRENT_USER, 0);
    let all_users = uninstall_location(HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY)
        .or_else(|| uninstall_location(HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY));
    mode_for(
        exe_dir.as_deref(),
        per_user.as_deref(),
        all_users.as_deref(),
    )
}

/// Setup's command line: silent, the install mode to keep, our process id
/// (Setup waits for it) and the arguments to start Dodavo with afterwards.
pub fn setup_parameters(
    pid: u32,
    mode: InstallMode,
    relaunch_args: &str,
    log: Option<&Path>,
) -> String {
    let mut params = String::from("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL");
    match mode {
        InstallMode::PerUser => params.push_str(" /CURRENTUSER"),
        InstallMode::AllUsers => params.push_str(" /ALLUSERS"),
        InstallMode::Unknown => {}
    }
    params.push_str(&format!(
        " /DODAVOUPDATE={pid} /DODAVOARGS=\"{}\"",
        relaunch_args.replace('"', "")
    ));
    // A log with a fixed name only when Setup runs as this user. An
    // elevated Setup may run as another account (someone else's
    // administrator credentials), which may not be able to create a file in
    // this user's temp folder, and Setup aborts when it cannot create the
    // named log. Plain /LOG writes into the installing account's own temp
    // folder instead and never fails.
    match log {
        Some(log) if mode == InstallMode::PerUser => {
            params.push_str(&format!(" /LOG=\"{}\"", log.display()));
        }
        _ => params.push_str(" /LOG"),
    }
    params
}

/// Starts Setup silently (see [`setup_parameters`]) and returns a handle of
/// its process. Setup is started with the plain "open" verb, never elevated
/// by Dodavo: for an all-users installation Setup asks for elevation itself,
/// and only then can it start Dodavo again as the ordinary user. The elevated
/// Setup tells every running instance to exit (`Dodavo_ExitForUpdate`) and
/// waits for them before it replaces the program.
pub fn launch_installer(
    setup: &Path,
    mode: InstallMode,
    relaunch_args: &str,
) -> Result<HANDLE, Error> {
    let log = log_path();
    if let Some(log) = &log {
        let _ = std::fs::remove_file(log);
    }
    let params = setup_parameters(std::process::id(), mode, relaunch_args, log.as_deref());
    let verb = wide("open");
    let file = wide(&setup.display().to_string());
    let params = wide(&params);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    if !ok || info.hProcess.is_null() {
        let code = unsafe { GetLastError() };
        crate::log_error!("Setup could not be started ({mode:?}): Windows error {code}");
        return Err(Error::Launch(format!(
            "{} (Windows error {code})",
            setup.display()
        )));
    }
    crate::log_info!("Setup started ({mode:?})");
    Ok(info.hProcess)
}

/// Whether the Setup process has ended already, and with which exit code.
// A handle is not dereferenced here; Windows checks it.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn setup_exited(process: HANDLE) -> Option<u32> {
    unsafe {
        if WaitForSingleObject(process, 0) != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        GetExitCodeProcess(process, &mut code);
        Some(code)
    }
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn close_handle(process: HANDLE) {
    unsafe { CloseHandle(process) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_mode_follows_the_registration_of_the_running_copy() {
        let pf = Path::new(r"C:\Program Files\Dodavo\");
        let local = Path::new(r"C:\Users\ana\AppData\Local\Programs\Dodavo\");
        let m = |exe: &str, per_user: Option<&Path>, all_users: Option<&Path>| {
            mode_for(Some(Path::new(exe)), per_user, all_users)
        };
        assert_eq!(
            m(r"C:\Program Files\Dodavo", None, Some(pf)),
            InstallMode::AllUsers
        );
        assert_eq!(
            m(r"c:\program files\dodavo", Some(local), Some(pf)),
            InstallMode::AllUsers
        );
        assert_eq!(
            m(
                r"C:\Users\ana\AppData\Local\Programs\Dodavo",
                Some(local),
                Some(pf)
            ),
            InstallMode::PerUser
        );
        assert_eq!(
            m(
                r"C:\Users\ana\AppData\Local\Programs\Dodavo",
                Some(local),
                None
            ),
            InstallMode::PerUser
        );
        // A portable copy next to an installation, or no installation at all.
        assert_eq!(
            m(r"D:\Tools\Dodavo", Some(local), Some(pf)),
            InstallMode::Unknown
        );
        assert_eq!(m(r"D:\Tools\Dodavo", None, None), InstallMode::Unknown);
        assert_eq!(mode_for(None, Some(local), Some(pf)), InstallMode::Unknown);
    }

    #[test]
    fn setup_parameters_name_the_mode_the_pid_and_the_relaunch() {
        let log = Path::new(r"C:\Temp\Dodavo\update\setup.log");
        let p = setup_parameters(
            4711,
            InstallMode::AllUsers,
            "--updated --profile ana",
            Some(log),
        );
        // Elevated Setup may run as another account: no fixed log path.
        assert_eq!(
            p,
            "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /NOCANCEL /ALLUSERS \
             /DODAVOUPDATE=4711 /DODAVOARGS=\"--updated --profile ana\" /LOG"
        );
        let p = setup_parameters(1, InstallMode::PerUser, "--updated", Some(log));
        assert!(p.contains(" /CURRENTUSER "), "{p}");
        assert!(
            p.ends_with("/DODAVOARGS=\"--updated\" /LOG=\"C:\\Temp\\Dodavo\\update\\setup.log\""),
            "{p}"
        );
        let p = setup_parameters(1, InstallMode::PerUser, "--updated", None);
        assert!(p.ends_with("/DODAVOARGS=\"--updated\" /LOG"), "{p}");
        let p = setup_parameters(
            1,
            InstallMode::Unknown,
            "--updated \"--minimized\"",
            Some(log),
        );
        assert!(!p.contains("USER"), "{p}");
        assert!(p.ends_with(" /LOG"), "{p}");
        // Quotes inside the arguments would break Setup's parsing; they are dropped.
        assert!(p.contains("/DODAVOARGS=\"--updated --minimized\""), "{p}");
    }
}
