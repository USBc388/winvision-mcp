//! Window enumeration and management (EnumWindows / ShowWindow / snap / launch).

use std::mem::size_of;

use windows::core::PWSTR;
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    GetProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowRect,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, SetForegroundWindow,
    SetWindowPos, ShowWindow, GA_ROOT, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL,
};

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub class: String,
    pub process: String,
    pub pid: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub state: &'static str,
}

impl WindowInfo {
    pub fn rect(&self) -> RECT {
        RECT {
            left: self.x,
            top: self.y,
            right: self.x + self.w,
            bottom: self.y + self.h,
        }
    }

    /// `[hwnd 0x1a2b, pid 1234] "Title" — app.exe (x, y, w, h) [minimized|maximized]`
    pub fn line(&self) -> String {
        let mut s = format!(
            "[hwnd 0x{:x}, pid {}] \"{}\" — {} (x: {}, y: {}, w: {}, h: {})",
            self.hwnd, self.pid, self.title, self.process, self.x, self.y, self.w, self.h
        );
        match self.state {
            "min" => s.push_str(" [minimized]"),
            "max" => s.push_str(" [maximized]"),
            _ => {}
        }
        s
    }
}

fn process_name(pid: u32) -> String {
    unsafe {
        let h = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL(0), pid) {
            Ok(h) => h,
            Err(_) => return "?".to_string(),
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let name = if QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        {
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string()
        } else {
            "?".to_string()
        };
        let _ = CloseHandle(h);
        name
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    unsafe {
        let mut cloaked: u32 = 0;
        let r = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut std::ffi::c_void,
            size_of::<u32>() as u32,
        );
        r.is_ok() && cloaked != 0
    }
}

fn state_of(hwnd: HWND) -> &'static str {
    unsafe {
        if IsIconic(hwnd).0 != 0 {
            "min"
        } else if IsZoomed(hwnd).0 != 0 {
            "max"
        } else {
            ""
        }
    }
}

pub fn window_info(hwnd: HWND) -> Option<WindowInfo> {
    unsafe {
        if IsWindow(hwnd).0 == 0 {
            return None;
        }
        let mut title = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
        let mut class = [0u16; 256];
        let nc = GetClassNameW(hwnd, &mut class);
        let class = String::from_utf16_lossy(&class[..nc.max(0) as usize]);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return None;
        }
        Some(WindowInfo {
            hwnd: hwnd.0 as isize,
            title,
            class,
            process: process_name(pid),
            pid,
            x: r.left,
            y: r.top,
            w: (r.right - r.left).max(0),
            h: (r.bottom - r.top).max(0),
            state: state_of(hwnd),
        })
    }
}

/// Enumerate visible, uncloaked top-level windows in Z order.
/// Empty-title windows are skipped unless `include_empty_title`.
pub fn collect_windows(include_empty_title: bool) -> Vec<WindowInfo> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let (out, include_empty) = &mut *(lparam.0 as *mut (Vec<WindowInfo>, bool));
        if IsWindowVisible(hwnd).0 == 0 {
            return BOOL(1);
        }
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return BOOL(1);
        }
        if is_cloaked(hwnd) {
            return BOOL(1);
        }
        if let Some(info) = window_info(hwnd) {
            if info.title.trim().is_empty() && !*include_empty {
                return BOOL(1);
            }
            if info.w <= 0 || info.h <= 0 {
                return BOOL(1);
            }
            out.push(info);
        }
        BOOL(1)
    }
    let mut pack = (Vec::new(), include_empty_title);
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut pack as *mut _ as isize));
    }
    pack.0
}

/// Find a window by hwnd number, exact/substring title or process name
/// (case-insensitive). Exact hwnd wins, then exact title, then title
/// substring, then process substring.
pub fn find_window(query: &str) -> Option<WindowInfo> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if let Some(hex) = q.strip_prefix("0x").or_else(|| q.strip_prefix("0X")) {
        if let Ok(h) = isize::from_str_radix(hex, 16) {
            if let Some(info) = window_info(HWND(h as *mut _)) {
                return Some(info);
            }
        }
    }
    if let Ok(h) = q.parse::<isize>() {
        if h != 0 {
            if let Some(info) = window_info(HWND(h as *mut _)) {
                return Some(info);
            }
        }
    }
    let ql = q.to_lowercase();
    let wins = collect_windows(false);
    if let Some(w) = wins.iter().find(|w| w.title.to_lowercase() == ql) {
        return Some(w.clone());
    }
    if let Some(w) = wins.iter().find(|w| w.title.to_lowercase().contains(&ql)) {
        return Some(w.clone());
    }
    wins.into_iter()
        .find(|w| w.process.to_lowercase().contains(&ql))
}

pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Bring a window to the foreground, working around the Windows foreground
/// lock with an Alt-key tap. Returns a human-readable status.
pub fn focus_window(hwnd: isize) -> Result<String, String> {
    let h = HWND(hwnd as *mut _);
    if unsafe { IsWindow(h).0 == 0 } {
        return Err(format!("hwnd 0x{hwnd:x} is not a valid window"));
    }
    unsafe {
        if IsIconic(h).0 != 0 {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if GetForegroundWindow() != h {
            crate::input::tap_alt();
        }
        let ok = SetForegroundWindow(h).0 != 0;
        std::thread::sleep(std::time::Duration::from_millis(60));
        let now = GetForegroundWindow();
        if now == h || ok {
            Ok(format!("focused hwnd 0x{hwnd:x}"))
        } else {
            Err(format!(
                "SetForegroundWindow did not stick for hwnd 0x{hwnd:x} (another window kept focus)"
            ))
        }
    }
}

pub fn set_window_state(hwnd: isize, action: &str) -> Result<String, String> {
    let h = HWND(hwnd as *mut _);
    let cmd = match action {
        "minimize" => SW_MINIMIZE,
        "maximize" => SW_MAXIMIZE,
        "restore" => SW_RESTORE,
        _ => return Err(format!("unknown window state action '{action}'")),
    };
    unsafe {
        let _ = ShowWindow(h, cmd);
        Ok(format!("{action} sent to hwnd 0x{hwnd:x}"))
    }
}

/// Snap a window to the left/right half (or full) of its current monitor.
pub fn snap_window(hwnd: isize, side: &str) -> Result<String, String> {
    let h = HWND(hwnd as *mut _);
    unsafe {
        let hm: HMONITOR = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hm, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO).0 == 0 {
            return Err("GetMonitorInfoW failed".into());
        }
        let rc = mi.monitorInfo.rcMonitor;
        if IsIconic(h).0 != 0 {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let (x, y, w, hh) = match side {
            "left" => (rc.left, rc.top, (rc.right - rc.left) / 2, rc.bottom - rc.top),
            "right" => (
                rc.left + (rc.right - rc.left) / 2,
                rc.top,
                (rc.right - rc.left) / 2,
                rc.bottom - rc.top,
            ),
            "full" => (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top),
            _ => return Err(format!("unknown snap side '{side}' (use left|right|full)")),
        };
        SetWindowPos(h, HWND_TOPMOST, x, y, w, hh, SWP_NOACTIVATE | SWP_SHOWWINDOW)
            .map_err(|e| format!("SetWindowPos failed: {e}"))?;
        SetWindowPos(
            h,
            HWND_NOTOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
        .map_err(|e| format!("SetWindowPos(untop) failed: {e}"))?;
        Ok(format!("snapped hwnd 0x{hwnd:x} {side} to ({x},{y},{w},{hh})"))
    }
}

/// Launch an application (ShellExecute "open"). Returns the new PID when
/// the shell reports one.
pub fn launch_app(command: &str, args: &str) -> Result<String, String> {
    let file = to_wide(command);
    let params = to_wide(args);
    unsafe {
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpFile: windows::core::PCWSTR::from_raw(file.as_ptr()),
            lpParameters: if args.is_empty() {
                windows::core::PCWSTR::null()
            } else {
                windows::core::PCWSTR::from_raw(params.as_ptr())
            },
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        ShellExecuteExW(&mut sei).map_err(|e| format!("ShellExecuteExW failed: {e}"))?;
        if !sei.hProcess.is_invalid() {
            let pid = GetProcessId(sei.hProcess);
            let _ = CloseHandle(sei.hProcess);
            Ok(format!("launched \"{command}\", pid {pid}"))
        } else {
            Ok(format!("launched \"{command}\" (no process handle)"))
        }
    }
}

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_wide_null_terminated() {
        assert_eq!(to_wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
    }

    #[test]
    fn find_window_empty_query_is_none() {
        assert!(find_window("").is_none());
        assert!(find_window("   ").is_none());
    }
}
