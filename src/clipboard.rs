//! CF_UNICODETEXT clipboard access (used for fast text entry via Ctrl+V).

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};

pub fn get_text() -> Result<String, String> {
    unsafe {
        OpenClipboard(HWND::default()).map_err(|e| format!("OpenClipboard failed: {e} (clipboard busy)"))?;
        let result = (|| -> Result<String, String> {
            IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32)
                .map_err(|e| format!("IsClipboardFormatAvailable failed: {e}"))?;
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32)
                .map_err(|_| "clipboard does not contain text".to_string())?;
            let hg = HGLOBAL(h.0);
            let size = GlobalSize(hg);
            let ptr = GlobalLock(hg);
            if ptr.is_null() {
                return Err("GlobalLock failed".into());
            }
            let u16s = size / 2;
            let slice = std::slice::from_raw_parts(ptr as *const u16, u16s);
            let len = slice.iter().position(|&c| c == 0).unwrap_or(u16s);
            let s = String::from_utf16_lossy(&slice[..len]);
            let _ = GlobalUnlock(hg);
            Ok(s)
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn set_text(s: &str) -> Result<String, String> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len() * 2;
    unsafe {
        OpenClipboard(HWND::default()).map_err(|e| format!("OpenClipboard failed: {e} (clipboard busy)"))?;
        let result = (|| -> Result<String, String> {
            EmptyClipboard().map_err(|e| format!("EmptyClipboard failed: {e}"))?;
            let hg = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| format!("GlobalAlloc failed: {e}"))?;
            let ptr = GlobalLock(hg);
            if ptr.is_null() {
                return Err("GlobalLock failed".into());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes);
            let _ = GlobalUnlock(hg);
            if SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hg.0)).is_err() {
                let _ = GlobalFree(hg);
                return Err("SetClipboardData failed".into());
            }
            Ok(format!("clipboard set ({len} chars)", len = s.chars().count()))
        })();
        let _ = CloseClipboard();
        result
    }
}
