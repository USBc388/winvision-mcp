//! Keyboard & mouse macros via SendInput: hotkeys, unicode typing, clicks,
//! smooth drag, wheel scrolling. All coordinates are physical screen pixels
//! across the whole virtual desktop (multi-monitor aware).

use std::time::Duration;

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, VK_A, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE, VK_DOWN,
    VK_END, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F13, VK_F14, VK_F15, VK_F16, VK_F17,
    VK_F18, VK_F19, VK_F2, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24, VK_F3, VK_F4, VK_F5, VK_F6,
    VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT, VK_LWIN, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    unsafe {
        let scan = MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) as u16;
        let mut flags = KEYBD_EVENT_FLAGS(KEYEVENTF_SCANCODE.0);
        if extended_key(vk) {
            flags.0 |= KEYEVENTF_EXTENDEDKEY.0;
        }
        if up {
            flags.0 |= KEYEVENTF_KEYUP.0;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}

fn unicode_input(ch: u16, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0);
    if up {
        flags.0 |= KEYEVENTF_KEYUP.0;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: ch,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(dx: i32, dy: i32, flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn extended_key(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        VK_UP | VK_DOWN | VK_LEFT | VK_RIGHT | VK_INSERT | VK_DELETE | VK_HOME | VK_END
            | VK_PRIOR | VK_NEXT
    )
}

/// Send raw INPUTs; error when the OS swallowed part of the batch.
fn send_all(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    unsafe {
        let sent = SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
        if sent as usize != inputs.len() {
            return Err(format!(
                "SendInput delivered {sent}/{} events — input may be blocked (UAC elevation / fullscreen)",
                inputs.len()
            ));
        }
    }
    Ok(())
}

pub fn tap_alt() {
    let _ = send_all(&[key_input(VK_MENU, false), key_input(VK_MENU, true)]);
}

/// Parse "Ctrl+Shift+P" / "Alt+Tab" / "Win+D" / "F5" into modifiers + key.
pub fn parse_hotkey(s: &str) -> Result<(Vec<VIRTUAL_KEY>, VIRTUAL_KEY), String> {
    let parts: Vec<String> = s
        .split('+')
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("empty hotkey string".into());
    }
    let mut mods = Vec::new();
    let mut main: Option<VIRTUAL_KEY> = None;
    for (i, p) in parts.iter().enumerate() {
        let vk = match p.as_str() {
            "ctrl" | "control" => VK_CONTROL,
            "alt" => VK_MENU,
            "shift" => VK_SHIFT,
            "win" | "meta" | "windows" => VK_LWIN,
            _ if i + 1 < parts.len() => {
                return Err(format!("unknown modifier '{p}' (use ctrl|alt|shift|win)"))
            }
            _ => map_key(p)?,
        };
        if i + 1 < parts.len() {
            mods.push(vk);
        } else {
            main = Some(vk);
        }
    }
    match main {
        Some(m) => {
            // A bare modifier ("Ctrl") is not a hotkey.
            if matches!(m, VK_CONTROL | VK_MENU | VK_SHIFT | VK_LWIN) {
                return Err(format!("'{s}' is only a modifier — add a non-modifier key"));
            }
            Ok((mods, m))
        }
        None => Err("hotkey needs a non-modifier key".into()),
    }
}

fn map_key(name: &str) -> Result<VIRTUAL_KEY, String> {
    let k = name.to_lowercase();
    let vk = match k.as_str() {
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "esc" | "escape" => VK_ESCAPE,
        "space" | "spacebar" => VK_SPACE,
        "backspace" | "bs" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "pgup" => VK_PRIOR,
        "pagedown" | "pgdn" => VK_NEXT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "capslock" | "caps" => VK_CAPITAL,
        "printscreen" | "prtsc" => VK_SNAPSHOT,
        _ if k.len() == 1 && k.as_bytes()[0].is_ascii_alphabetic() => {
            VIRTUAL_KEY(VK_A.0 + (k.as_bytes()[0] - b'a') as u16)
        }
        _ if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => {
            VIRTUAL_KEY(0x30 + (k.as_bytes()[0] - b'0') as u16)
        }
        _ if k.starts_with('f') && k[1..].parse::<u16>().is_ok() => {
            let n: u16 = k[1..].parse().unwrap();
            if !(1..=24).contains(&n) {
                return Err(format!("F{n} out of range (F1..F24)"));
            }
            match n {
                1 => VK_F1,
                2 => VK_F2,
                3 => VK_F3,
                4 => VK_F4,
                5 => VK_F5,
                6 => VK_F6,
                7 => VK_F7,
                8 => VK_F8,
                9 => VK_F9,
                10 => VK_F10,
                11 => VK_F11,
                12 => VK_F12,
                13 => VK_F13,
                14 => VK_F14,
                15 => VK_F15,
                16 => VK_F16,
                17 => VK_F17,
                18 => VK_F18,
                19 => VK_F19,
                20 => VK_F20,
                21 => VK_F21,
                22 => VK_F22,
                23 => VK_F23,
                _ => VK_F24,
            }
        }
        _ => return Err(format!("unknown key '{name}'")),
    };
    Ok(vk)
}

/// Press a chord: modifiers down, key down/up, modifiers up (reversed).
pub fn send_hotkey(s: &str) -> Result<String, String> {
    let (mods, key) = parse_hotkey(s)?;
    let mut inputs = Vec::new();
    for m in &mods {
        inputs.push(key_input(*m, false));
    }
    inputs.push(key_input(key, false));
    inputs.push(key_input(key, true));
    for m in mods.iter().rev() {
        inputs.push(key_input(*m, true));
    }
    send_all(&inputs)?;
    Ok(format!("sent hotkey {s}"))
}

/// Type text as unicode key events (works in most apps without focus tricks).
pub fn type_text(text: &str) -> Result<String, String> {
    let mut inputs = Vec::new();
    for ch in text.chars() {
        if ch == '\n' {
            inputs.push(key_input(VK_RETURN, false));
            inputs.push(key_input(VK_RETURN, true));
            continue;
        }
        if ch == '\t' {
            inputs.push(key_input(VK_TAB, false));
            inputs.push(key_input(VK_TAB, true));
            continue;
        }
        let mut buf = [0u16; 2];
        for unit in ch.encode_utf16(&mut buf) {
            inputs.push(unicode_input(*unit, false));
            inputs.push(unicode_input(*unit, true));
        }
    }
    send_all(&inputs)?;
    Ok(format!("typed {} chars", text.chars().count()))
}

fn virtual_to_absolute(x: i32, y: i32) -> (i32, i32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        let ax = ((x - vx) * 65535) / (vw - 1).max(1);
        let ay = ((y - vy) * 65535) / (vh - 1).max(1);
        (ax.clamp(0, 65535), ay.clamp(0, 65535))
    }
}

/// Smoothly move the cursor to (x, y) over a few interpolated steps.
pub fn move_mouse(x: i32, y: i32, smooth: bool) -> Result<(), String> {
    let (ax, ay) = virtual_to_absolute(x, y);
    let move_flags = MOUSE_EVENT_FLAGS(
        MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0,
    );
    if !smooth {
        return send_all(&[mouse_input(ax, ay, move_flags, 0)]);
    }
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let (cx, cy) = virtual_to_absolute(p.x, p.y);
        let mut inputs = Vec::new();
        for i in 1..=8 {
            let t = i as f32 / 8.0;
            let nx = (cx as f32 + (ax - cx) as f32 * t) as i32;
            let ny = (cy as f32 + (ay - cy) as f32 * t) as i32;
            inputs.push(mouse_input(nx, ny, move_flags, 0));
        }
        send_all(&inputs)?;
    }
    Ok(())
}

fn button_flags(button: &str, down: bool) -> Result<MOUSE_EVENT_FLAGS, String> {
    Ok(match (button, down) {
        ("left", true) => MOUSEEVENTF_LEFTDOWN,
        ("left", false) => MOUSEEVENTF_LEFTUP,
        ("right", true) => MOUSEEVENTF_RIGHTDOWN,
        ("right", false) => MOUSEEVENTF_RIGHTUP,
        ("middle", true) => MOUSEEVENTF_MIDDLEDOWN,
        ("middle", false) => MOUSEEVENTF_MIDDLEUP,
        _ => return Err(format!("unknown mouse button '{button}' (left|right|middle)")),
    })
}

/// Hardware click at physical screen coordinates.
pub fn click_at(x: i32, y: i32, button: &str, clicks: u32) -> Result<String, String> {
    move_mouse(x, y, true)?;
    std::thread::sleep(Duration::from_millis(40));
    let mut inputs = Vec::new();
    for _ in 0..clicks.max(1) {
        inputs.push(mouse_input(0, 0, button_flags(button, true)?, 0));
        inputs.push(mouse_input(0, 0, button_flags(button, false)?, 0));
    }
    send_all(&inputs)?;
    Ok(format!("{clicks}x {button} click at ({x},{y})"))
}

/// Press-and-hold drag from (x1,y1) to (x2,y2) with interpolated path.
pub fn drag(x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u32) -> Result<String, String> {
    let move_flags = MOUSE_EVENT_FLAGS(
        MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0,
    );
    move_mouse(x1, y1, true)?;
    std::thread::sleep(Duration::from_millis(60));
    send_all(&[mouse_input(0, 0, MOUSEEVENTF_LEFTDOWN, 0)])?;
    let steps = (duration_ms.max(60) / 10).clamp(6, 40) as f32;
    for i in 1..=(steps as i32) {
        let t = i as f32 / steps;
        let px = x1 + ((x2 - x1) as f32 * t) as i32;
        let py = y1 + ((y2 - y1) as f32 * t) as i32;
        let (ax, ay) = virtual_to_absolute(px, py);
        send_all(&[mouse_input(ax, ay, move_flags, 0)])?;
        std::thread::sleep(Duration::from_millis(
            (duration_ms.max(60) / steps as u32).max(5) as u64,
        ));
    }
    std::thread::sleep(Duration::from_millis(40));
    send_all(&[mouse_input(0, 0, MOUSEEVENTF_LEFTUP, 0)])?;
    Ok(format!("dragged ({x1},{y1}) -> ({x2},{y2}) in {duration_ms}ms"))
}

/// Wheel scroll; positive delta scrolls up, one notch = 120.
pub fn scroll(x: Option<i32>, y: Option<i32>, delta: i32) -> Result<String, String> {
    if let (Some(x), Some(y)) = (x, y) {
        move_mouse(x, y, false)?;
        std::thread::sleep(Duration::from_millis(30));
    }
    let mut inputs = Vec::new();
    let mut left = delta;
    while left != 0 {
        let step = left.clamp(-120 * 10, 120 * 10);
        inputs.push(mouse_input(0, 0, MOUSEEVENTF_WHEEL, step as u32));
        left -= step;
    }
    send_all(&inputs)?;
    Ok(format!("scrolled {delta} at {:?}", x.zip(y)))
}

/// Hardware click on the center of a screen rect (UIA fallback path).
pub fn click_rect_center(
    rect: (f64, f64, f64, f64),
    button: &str,
    clicks: u32,
) -> Result<String, String> {
    let (l, t, w, h) = rect;
    let cx = (l + w / 2.0).round() as i32;
    let cy = (t + h / 2.0).round() as i32;
    click_at(cx, cy, button, clicks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctrl_shift_p() {
        let (mods, key) = parse_hotkey("Ctrl+Shift+P").unwrap();
        assert_eq!(mods, vec![VK_CONTROL, VK_SHIFT]);
        assert_eq!(key.0, VK_A.0 + (b'p' - b'a') as u16);
    }

    #[test]
    fn parses_alt_tab() {
        let (mods, key) = parse_hotkey("Alt+Tab").unwrap();
        assert_eq!(mods, vec![VK_MENU]);
        assert_eq!(key, VK_TAB);
    }

    #[test]
    fn parses_f5_and_win_d() {
        assert_eq!(parse_hotkey("f5").unwrap().1, VK_F5);
        let (mods, key) = parse_hotkey("Win+d").unwrap();
        assert_eq!(mods, vec![VK_LWIN]);
        assert_eq!(key.0, VK_A.0 + (b'd' - b'a') as u16);
    }

    #[test]
    fn parses_arrows_and_specials() {
        assert_eq!(parse_hotkey("Ctrl+PgDn").unwrap().1, VK_NEXT);
        assert_eq!(parse_hotkey("ESC").unwrap().1, VK_ESCAPE);
        assert_eq!(parse_hotkey("Enter").unwrap().1, VK_RETURN);
        assert_eq!(parse_hotkey("PrintScreen").unwrap().1, VK_SNAPSHOT);
    }

    #[test]
    fn rejects_bad_hotkeys() {
        assert!(parse_hotkey("").is_err());
        assert!(parse_hotkey("Ctrl").is_err()); // only a modifier
        assert!(parse_hotkey("Ctrl+~~").is_err());
        assert!(parse_hotkey("F30").is_err());
    }

    #[test]
    fn digit_keys_map_correctly() {
        assert_eq!(parse_hotkey("5").unwrap().1.0, 0x35);
    }

    #[test]
    fn virtual_to_absolute_inside_desktop() {
        let (ax, _ay) = virtual_to_absolute(0, 0);
        assert!((0..=65535).contains(&ax));
    }
}
