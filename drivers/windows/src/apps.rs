//! Resolving an `AppSelector` to a real pid via windowed processes.
//!
//! Name and AUMID lookups only consider pids that own a top-level
//! window — a windowless process has no UIA tree to observe, so
//! matching it would scope an observation that cannot see anything.

use dexter_core::AppSelector;
use dexter_driver::DriverError;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WIN32_ERROR};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The one pid behind an app selector. `Name` matches a windowed
/// process' image stem or file name case-insensitively, `BundleId`
/// the AppUserModelId; several running matches fail closed as
/// `Ambiguous` via `unique_app_pid`.
pub fn resolve_pid(selector: &AppSelector) -> Result<i32, DriverError> {
    match selector {
        AppSelector::Pid(pid) => Ok(*pid),
        AppSelector::Name(name) => {
            let pids: Vec<i32> = crate::win::windowed_pids()
                .into_iter()
                .filter(|pid| {
                    process_image_path(*pid).is_some_and(|path| {
                        let (stem, file) = split_image_name(&path);
                        stem.eq_ignore_ascii_case(name) || file.eq_ignore_ascii_case(name)
                    })
                })
                .collect();
            dexter_driver::unique_app_pid(&pids, &format!("name '{name}'"))
        }
        AppSelector::BundleId(bundle) => {
            let pids: Vec<i32> = crate::win::windowed_pids()
                .into_iter()
                .filter(|pid| aumid_for_pid(*pid).is_some_and(|a| a.eq_ignore_ascii_case(bundle)))
                .collect();
            dexter_driver::unique_app_pid(&pids, &format!("bundle id '{bundle}'"))
        }
    }
}

/// `"C:\Windows\System32\notepad.exe"` → `("notepad", "notepad.exe")` —
/// the stem is the app name a user means; the file name is kept so a
/// selector written either way matches.
fn split_image_name(path: &str) -> (&str, &str) {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = file
        .get(file.len().saturating_sub(4)..)
        .filter(|ext| ext.eq_ignore_ascii_case(".exe"))
        .map_or(file, |_| &file[..file.len() - 4]);
    (stem, file)
}

/// Owning-process image path for `pid`, or `None` when the process
/// can't be opened/read (dead pid, protected process).
fn process_image_path(pid: i32) -> Option<String> {
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32).ok()?;
        let mut len = 512u32;
        let mut buf = vec![0u16; len as usize];
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Process image stem (`notepad.exe` → `notepad`) for `pid`.
pub fn process_name(pid: i32) -> Option<String> {
    process_image_path(pid).map(|p| split_image_name(&p).0.to_string())
}

/// AppUserModelId for `pid` — the honest `bundle_id` on Windows.
/// Packaged apps (Store/MSIX) carry one; `None` for everything else.
pub fn aumid_for_pid(pid: i32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32).ok()?;
        let mut len = 512u32;
        let mut buf = vec![0u16; len as usize];
        let mut err: WIN32_ERROR =
            GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        if err == windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER {
            buf = vec![0u16; len as usize];
            err = GetApplicationUserModelId(h, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        }
        let _ = CloseHandle(h);
        if err != windows::Win32::Foundation::ERROR_SUCCESS {
            return None;
        }
        // `len` counts the NUL terminator.
        (len > 0).then(|| String::from_utf16_lossy(&buf[..(len - 1) as usize]))
    }
}
