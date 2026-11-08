//! Explorer, memory, process enumeration and window activation (Windows).

use std::os::windows::process::CommandExt;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow};
use windows::core::w;

use super::wide::{Wide, from_buf};

/// The arguments C# passes to `explorer.exe` (`/select,"<path>"` or `"<path>"`).
pub fn explorer_arguments(path: &Path, select: bool) -> String {
    let p = path.to_string_lossy();
    if select { format!("/select,\"{p}\"") } else { format!("\"{p}\"") }
}

/// `explorer.exe /select,"path"` (reveal) or `explorer.exe "path"` (open); errors ignored.
pub fn open_in_explorer(path: &Path, select: bool) {
    if path.as_os_str().is_empty() {
        return;
    }
    let _ = std::process::Command::new("explorer.exe").raw_arg(explorer_arguments(path, select)).spawn();
}

/// `ms-settings:defaultapps` (Settings ▸ Default apps).
pub fn open_default_apps_settings() {
    unsafe {
        ShellExecuteW(None, w!("open"), w!("ms-settings:defaultapps"), None, None, SW_SHOWNORMAL);
    }
}

/// Opens a URL or document with its default handler.
pub fn shell_open(target: &str) {
    let t = Wide::new(target);
    unsafe {
        ShellExecuteW(None, w!("open"), t.pcwstr(), None, None, SW_SHOWNORMAL);
    }
}

/// `GlobalMemoryStatusEx().ullTotalPhys`.
pub fn total_physical_memory() -> u64 {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    if unsafe { GlobalMemoryStatusEx(&mut m) }.is_ok() { m.ullTotalPhys } else { 0 }
}

/// `(pid, exe file name)` of every process.
pub fn processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                out.push((e.th32ProcessID, from_buf(&e.szExeFile)));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// `Process.GetProcessesByName(name)` semantics: the exe name without `.exe`, case-insensitive.
pub fn process_name_matches(exe_file: &str, name: &str) -> bool {
    let lower = exe_file.to_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    stem == name.to_lowercase()
}

/// C# `IsAnotherInstanceOrEngineRunning`: another `SubtitleAdder` process, or any `subtitle-engine`.
pub fn other_app_instances_running() -> bool {
    let me = std::process::id();
    processes().iter().any(|(pid, exe)| {
        (process_name_matches(exe, "SubtitleAdder") && *pid != me) || process_name_matches(exe, "subtitle-engine")
    })
}

/// C# `SingleInstanceCoordinator.BringToForeground`: restore when minimized, then `SetForegroundWindow`.
pub fn bring_to_foreground(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    let h = HWND(hwnd as *mut _);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_arguments_match_the_csharp_strings() {
        assert_eq!(explorer_arguments(Path::new(r"C:\v\a b.mp4"), true), r#"/select,"C:\v\a b.mp4""#);
        assert_eq!(explorer_arguments(Path::new(r"C:\v"), false), r#""C:\v""#);
    }

    #[test]
    fn process_names_compare_like_get_processes_by_name() {
        assert!(process_name_matches("SubtitleAdder.exe", "SubtitleAdder"));
        assert!(process_name_matches("subtitleadder.EXE", "SubtitleAdder"));
        assert!(process_name_matches("subtitle-engine.exe", "subtitle-engine"));
        assert!(!process_name_matches("SubtitleAdderX.exe", "SubtitleAdder"));
    }

    #[test]
    fn memory_and_processes_are_read() {
        assert!(total_physical_memory() > 1 << 30);
        let me = std::process::id();
        assert!(processes().iter().any(|(p, _)| *p == me));
    }
}
