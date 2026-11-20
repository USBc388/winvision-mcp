//! Event observer backed by SetWinEventHook (WINEVENT_OUTOFCONTEXT).
//!
//! The hook is installed on the COM actor thread which pumps Windows
//! messages; events are recorded into a bounded ring buffer that any
//! thread can read (used by `window_manager` action `recent_events`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{HWND, HMODULE};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowTextW, OBJID_CLIENT, OBJID_WINDOW, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, GA_ROOT,
};

#[derive(Clone, Debug)]
pub struct EventRec {
    /// Unix epoch milliseconds when the event arrived.
    pub at_ms: u128,
    /// Monotonic instant (for elapsed computations).
    pub at: Instant,
    pub kind: &'static str,
    pub hwnd: isize,
    pub title: String,
}

pub struct EventState {
    buf: Mutex<VecDeque<EventRec>>,
}

impl EventState {
    const CAP: usize = 128;

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            buf: Mutex::new(VecDeque::with_capacity(Self::CAP)),
        })
    }

    pub fn push(&self, kind: &'static str, hwnd: isize, title: String) {
        let mut b = self.buf.lock().unwrap();
        if b.len() >= Self::CAP {
            b.pop_front();
        }
        b.push_back(EventRec {
            at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            at: Instant::now(),
            kind,
            hwnd,
            title,
        });
    }

    /// Most recent events, newest first.
    pub fn recent(&self, n: usize) -> Vec<EventRec> {
        let b = self.buf.lock().unwrap();
        b.iter().rev().take(n).cloned().collect()
    }
}

fn event_name(ev: u32) -> &'static str {
    match ev {
        EVENT_SYSTEM_FOREGROUND => "foreground",
        EVENT_OBJECT_SHOW => "show",
        EVENT_OBJECT_DESTROY => "destroy",
        EVENT_OBJECT_NAMECHANGE => "namechange",
        _ => "other",
    }
}

unsafe extern "system" fn win_event_cb(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Cheap pre-filter: only top-level window events interest us.
    if id_child != 0 || (id_object != OBJID_WINDOW.0 && id_object != OBJID_CLIENT.0) {
        return;
    }
    if hwnd.0.is_null() {
        return;
    }
    if GetAncestor(hwnd, GA_ROOT) != hwnd {
        return;
    }
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, &mut buf);
    let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    STATE.with(|s| s.push(event_name(event), hwnd.0 as isize, title));
}

thread_local! {
    static STATE: Arc<EventState> = EventState::new();
}

/// Install hooks on the current thread. Returns the shared EventState used
/// by the callbacks. The calling thread MUST pump Windows messages.
pub fn install_hooks() -> Arc<EventState> {
    let state = STATE.with(|s| s.clone());
    unsafe {
        let _fg = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            HMODULE::default(),
            Some(win_event_cb),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        let _win = SetWinEventHook(
            EVENT_OBJECT_DESTROY, // 0x8001 .. 0x800C covers show/destroy/namechange
            EVENT_OBJECT_NAMECHANGE,
            HMODULE::default(),
            Some(win_event_cb),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
    }
    state
}
