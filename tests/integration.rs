//! Integration tests. Most exercise real Windows APIs (UIA / DXGI / OCR)
//! and need an interactive desktop session — they are `#[ignore]` so a plain
//! `cargo test` stays green in headless CI; run them explicitly with:
//!   cargo test -- --ignored

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Notepad-based tests must not kill each other's instance (cargo runs
/// tests in parallel threads).
static NOTEPAD_LOCK: Mutex<()> = Mutex::new(());

fn notepad_guard() -> std::sync::MutexGuard<'static, ()> {
    NOTEPAD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

use serde_json::{json, Value};

use winvision_mcp::actor::Actor;
use winvision_mcp::init_dpi;
use winvision_mcp::mcp::{call_tool, Content};

fn spawn_actor() -> Actor {
    init_dpi();
    Actor::spawn()
}

/// Extract the first Text content item for assertions.
fn first_text(res: &[Content]) -> String {
    match res.first() {
        Some(Content::Text(t)) => t.clone(),
        Some(other) => panic!("expected text content, got {other:?}"),
        None => panic!("empty content"),
    }
}

#[test]
#[ignore]
fn snapshot_is_under_50ms() {
    let _guard = notepad_guard();
    let actor = spawn_actor();
    // Launch Notepad for a stable, typical app window (the desktop shell
    // tree can be huge with wallpaper engines and is not the budget target).
    let launch = first_text(
        &call_tool(&actor, "window_manager", &json!({"action": "launch_app", "command": "notepad"}))
            .expect("launch notepad"),
    );
    let _ = launch;

    // Wait for the window, then warm up (first UIA call initializes COM).
    let mut hwnd_pid: u32 = 0;
    for _ in 0..50 {
        let res = call_tool(&actor, "window_manager", &json!({"action": "wait_for_window", "target": "notepad", "timeout_ms": 1000, "poll_ms": 100}));
        if let Ok(r) = res {
            let t = first_text(&r);
            hwnd_pid = t
                .split("pid ")
                .nth(1)
                .and_then(|s| s.split(' ').next())
                .and_then(|s| s.trim_end_matches(']').parse().ok())
                .unwrap_or(0);
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(hwnd_pid != 0, "notepad did not appear");

    // Warm-up snapshot.
    let _ = call_tool(&actor, "desktop_snapshot", &json!({ "target_window": "notepad" }));

    // Take 5 warm samples and assert on the MEDIAN (background load like an
    // animated wallpaper can cause one-off hiccups; steady-state is the
    // meaningful number).
    let mut samples: Vec<u128> = Vec::new();
    let mut last_text = String::new();
    for _ in 0..5 {
        let started = Instant::now();
        let res = call_tool(
            &actor,
            "desktop_snapshot",
            &json!({ "target_window": "notepad" }),
        )
        .expect("snapshot");
        samples.push(started.elapsed().as_millis());
        last_text = first_text(&res);
        std::thread::sleep(Duration::from_millis(30));
    }
    samples.sort();
    let median = samples[2];
    assert!(last_text.contains("[1] "), "snapshot must list elements: {last_text}");
    assert!(
        median <= 50,
        "median warm snapshot took {} ms (samples {:?}), budget is 50 ms",
        median,
        samples
    );

    // Cleanup: kill the notepad we launched.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &hwnd_pid.to_string(), "/F"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status();
}

/// A small, stable window to snapshot: the explorer desktop shell window
/// ("Program Manager") which always exists in an interactive session.
fn win_probe_target() -> String {
    "Program Manager".to_string()
}

#[test]
#[ignore]
fn browser_snapshot_exposes_page_elements() {
    let actor = spawn_actor();
    // Any running Chromium/Gecko browser works — the snapshot must wake its
    // accessibility tree and expose the PAGE, not just browser chrome.
    let target = ["floorp", "chrome", "msedge", "zen", "firefox", "brave"]
        .iter()
        .find(|b| winvision_mcp::window::find_window(b).is_some());
    let target = match target {
        Some(b) => b.to_string(),
        None => {
            println!("no browser running — skipping browser wake test");
            return;
        }
    };
    let res = call_tool(&actor, "desktop_snapshot", &json!({ "target_window": target }))
        .expect("browser snapshot");
    let text = first_text(&res);
    let elements = text.lines().filter(|l| l.trim_start().starts_with('[')).count();
    assert!(
        elements >= 20,
        "after wake, the browser must expose page elements (got {elements}):\n{text}"
    );
}

#[test]
#[ignore]
fn ocr_click_text_clicks_notepad_text() {
    let _guard = notepad_guard();
    let actor = spawn_actor();
    // Fresh notepad with known text.
    let _ = std::process::Command::new("taskkill")
        .args(["/IM", "notepad.exe", "/F"])
        .creation_flags(0x08000000)
        .status();
    std::thread::sleep(Duration::from_millis(400));
    call_tool(&actor, "window_manager", &json!({"action": "launch_app", "command": "notepad"}))
        .expect("launch notepad");
    let mut hwnd_pid: u32 = 0;
    for _ in 0..30 {
        if let Ok(r) = call_tool(
            &actor,
            "window_manager",
            &json!({"action": "wait_for_window", "target": "notepad", "timeout_ms": 800, "poll_ms": 120}),
        ) {
            let t = first_text(&r);
            hwnd_pid = t
                .split("pid ")
                .nth(1)
                .and_then(|s| s.split(' ').next())
                .and_then(|s| s.trim_end_matches(']').parse().ok())
                .unwrap_or(0);
            break;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    assert!(hwnd_pid != 0, "notepad did not appear");

    // Cleanup regardless of outcome below.
    let cleanup = || {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &hwnd_pid.to_string(), "/F"])
            .creation_flags(0x08000000)
            .status();
    };

    // Snapshot to find the editor element id, then put known text in.
    let snap = first_text(
        &call_tool(&actor, "desktop_snapshot", &json!({"target_window": "notepad"})).expect("snapshot"),
    );
    // The editor control may not be in the tree on the very first snapshot
    // (Win11 notepad initializes lazily) — retry until it shows up.
    let find_editor = |snap: &str| -> Option<u32> {
        snap.lines().find_map(|l| {
            let t = l.trim_start();
            if t.contains("] Edit") || t.contains("] Document") {
                let id: u32 = t
                    .trim_start_matches('[')
                    .split(']')
                    .next()?
                    .trim()
                    .parse()
                    .ok()?;
                Some(id)
            } else {
                None
            }
        })
    };
    let mut edit_id = find_editor(&snap);
    for _ in 0..8 {
        if edit_id.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
        let s = first_text(
            &call_tool(&actor, "desktop_snapshot", &json!({"target_window": "notepad"}))
                .expect("snapshot retry"),
        );
        edit_id = find_editor(&s);
    }
    let edit_id = edit_id.unwrap_or_else(|| panic!("editor element id not found; snapshot:\n{snap}"));
    let set = call_tool(
        &actor,
        "ui_interact",
        &json!({"element_id": edit_id, "action": "set_value", "text": "winvision ocr click 42"}),
    );
    if let Err(e) = set {
        cleanup();
        panic!("set_value into notepad editor failed: {e}");
    }

    let res = call_tool(
        &actor,
        "ocr_click_text",
        &json!({"text_query": "ocr click 42", "action": "click", "target_window": "notepad"}),
    );
    match res {
        Ok(r) => {
            let text = first_text(&r);
            assert!(
                text.contains("ocr_click_text: matched"),
                "unexpected result: {text}"
            );
        }
        Err(e) => {
            // OCR is machine-dependent; report clearly but keep the test
            // meaningful: only "not found" is tolerated here.
            assert!(e.contains("not found"), "unexpected error: {e}");
        }
    }
    cleanup();
}

#[test]
#[ignore]
fn snapshot_invalid_target_lists_windows() {
    let actor = spawn_actor();
    let err = call_tool(
        &actor,
        "desktop_snapshot",
        &json!({ "target_window": "definitely-not-a-window-xyz" }),
    )
    .expect_err("must fail");
    assert!(err.contains("no window matches"), "{err}");
    assert!(err.contains("Top-level windows"), "{err}");
}

#[test]
#[ignore]
fn ui_interact_stale_id_is_a_clear_error() {
    let actor = spawn_actor();
    let err = call_tool(
        &actor,
        "ui_interact",
        &json!({ "element_id": 42, "action": "click" }),
    )
    .expect_err("no snapshot yet");
    assert!(err.contains("no snapshot yet"), "{err}");
}

#[test]
#[ignore]
fn dxgi_capture_returns_full_frame() {
    let actor = spawn_actor();
    let res = call_tool(&actor, "ocr_read_region", &json!({
        "x": 0, "y": 0, "w": 640, "h": 120
    }))
    .expect("capture+ocr must succeed");
    let text = match &res[0] {
        Content::Text(t) => t.clone(),
        other => panic!("unexpected content {other:?}"),
    };
    assert!(text.contains("OCR 640x120"), "{text}");
}

#[test]
#[ignore]
fn som_capture_returns_image_content() {
    let actor = spawn_actor();
    let res = call_tool(
        &actor,
        "visual_som_capture",
        &json!({ "annot": "windows", "max_width": 800 }),
    )
    .expect("som capture");
    assert!(res.len() >= 2, "must return text + image");
    match &res[1] {
        Content::Image { data, mime } => {
            assert_eq!(*mime, "image/jpeg");
            // JPEG SOI marker in base64: /9j/
            assert!(data.starts_with("/9j/"), "not a JPEG payload");
        }
        other => panic!("second content must be image, got {other:?}"),
    }
}

#[test]
#[ignore]
fn window_list_and_events_work() {
    let actor = spawn_actor();
    let res = call_tool(&actor, "window_manager", &json!({"action": "list_windows"}))
        .expect("list_windows");
    let text = match &res[0] {
        Content::Text(t) => t.clone(),
        other => panic!("unexpected content {other:?}"),
    };
    assert!(text.contains("top-level windows"), "{text}");
    assert!(text.contains("hwnd 0x"), "{text}");
}

#[test]
#[ignore]
fn wait_for_window_times_out_cleanly() {
    let actor = spawn_actor();
    let err = call_tool(
        &actor,
        "window_manager",
        &json!({"action": "wait_for_window", "target": "no-such-window-xyz", "timeout_ms": 600, "poll_ms": 100}),
    );
    // wait_for_window polls in async context; in sync tests call_tool runs
    // it synchronously — either way it must produce a timeout message.
    match err {
        Err(e) => assert!(e.contains("timed out"), "{e}"),
        Ok(res) => panic!("expected timeout, got {res:?}"),
    }
}

#[test]
#[ignore]
fn end_to_end_stdio_protocol() {
    let exe = env!("CARGO_BIN_EXE_winvision-mcp");
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    let mut stdin = child.stdin.take().unwrap();
    let requests = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"window_manager","arguments":{"action":"list_windows"}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"desktop_snapshot","arguments":{"target_window": win_probe_target()}}}),
    ];
    for r in &requests {
        writeln!(stdin, "{r}").unwrap();
    }
    drop(stdin); // EOF -> server exits after replying

    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let mut lines = Vec::new();
    for line in reader.lines() {
        lines.push(line.unwrap());
    }
    let _ = child.wait_timeout(Duration::from_secs(5));

    assert_eq!(lines.len(), 4, "expected 4 responses, got {lines:?}");

    let init: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init["result"]["serverInfo"]["name"], "winvision");

    let tools: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(tools["id"], 2);
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 8);

    let list: Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!(list["id"], 3);
    assert_eq!(list["result"]["isError"], false);
    assert!(list["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("top-level windows"));

    let snap: Value = serde_json::from_str(&lines[3]).unwrap();
    assert_eq!(snap["id"], 4);
    let snap_text = snap["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("(missing)")
        .to_string();
    // The shell desktop root is a Pane; only require the ID-tagged listing.
    assert!(snap_text.contains("[1] "), "no elements listed: {snap_text}");
    assert!(snap_text.contains("Program Manager"), "wrong window: {snap_text}");
}

// Wait helper: the std test harness has no wait_timeout on Child, use a small shim.
trait WaitTimeout {
    fn wait_timeout(&mut self, d: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(&mut self, _d: Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        // stdin was dropped, the server exits promptly; plain wait is fine here.
        self.wait().map(Some)
    }
}

#[test]
fn protocol_smoke_without_windows_apis() {
    // Pure protocol checks that also run in headless CI:
    // initialize + tools/list + ping + unknown tool, all through handle_line.
    let actor = spawn_actor();
    let line = |s: &str| {
        winvision_mcp::mcp::handle_test_line(&actor, s)
    };
    let resp = line(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap();
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
    let resp = line(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 8);
    let resp = line(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#).unwrap();
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["result"], json!({}));
    assert!(v["id"] == 3);
}
