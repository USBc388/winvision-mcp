//! WinVision — local Windows vision and desktop control MCP server (stdio JSON-RPC 2.0).
//!
//! Modules:
//! - [`mcp`]: MCP JSON-RPC 2.0 transport over stdio (newline-delimited).
//! - [`actor`]: dedicated COM thread that owns all Windows API state.
//! - [`uia`]: UI Automation tree walker (desktop_snapshot / ui_interact).
//! - [`dxgi`]: DXGI Desktop Duplication frame grabber (<5 ms capture).
//! - [`som`]: Set-of-Mark annotation drawing (badges + grid).
//! - [`ocr`]: Windows.Media.Ocr offline text recognition.
//! - [`window`]: window enumeration / management.
//! - [`events`]: SetWinEventHook-based event observer.
//! - [`input`]: SendInput keyboard/mouse macros.
//! - [`clipboard`]: CF_UNICODETEXT clipboard access.

pub mod actor;
pub mod clipboard;
pub mod dxgi;
pub mod events;
pub mod input;
pub mod macros;
pub mod mcp;
pub mod ocr;
pub mod som;
pub mod uia;
pub mod window;

use std::sync::Once;

static DPI_INIT: Once = Once::new();

/// Make the process per-monitor DPI aware so all coordinates and captured
/// frames are true physical pixels. Must run before any window/capture work.
pub fn init_dpi() {
    DPI_INIT.call_once(|| unsafe {
        // Ignore failure (e.g. awareness already set via manifest).
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    });
}
