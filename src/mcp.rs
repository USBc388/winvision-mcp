//! MCP (Model Context Protocol) over stdio: newline-delimited JSON-RPC 2.0.
//!
//! Implements `initialize`, `tools/list`, `tools/call`, `ping`.
//! Tool execution errors are returned as results with `isError: true`
//! (per MCP spec); protocol errors use JSON-RPC error codes.

use std::io::Write;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::actor::Actor;
use crate::window;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

#[derive(Debug, Clone)]
pub enum Content {
    Text(String),
    Image {
        data: String,
        mime: &'static str,
    },
}

impl Content {
    fn to_json(&self) -> Value {
        match self {
            Content::Text(t) => json!({"type": "text", "text": t}),
            Content::Image { data, mime } => {
                json!({"type": "image", "data": data, "mimeType": mime})
            }
        }
    }
}

pub async fn run(actor: Actor) -> std::io::Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(&actor, &line);
        if let Some(resp) = response {
            let mut out = std::io::stdout().lock();
            writeln!(out, "{resp}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn write_json(v: &Value) -> String {
    // Single-line JSON — stdio transport forbids embedded newlines.
    serde_json::to_string(v).unwrap_or_else(|_| "{}".into())
}

/// Test/protocol debugging hook: run one JSON-RPC line through the handler.
pub fn handle_test_line(actor: &Actor, line: &str) -> Option<String> {
    handle_line(actor, line)
}

fn handle_line(actor: &Actor, line: &str) -> Option<String> {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(write_json(&json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": format!("Parse error: {e}")}
            })));
        }
    };
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // Notifications (no id) never get a response.
    let id = v.get("id").cloned();
    if method.starts_with("notifications/") || id.is_none() {
        return None;
    }
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    let id = id.unwrap();
    let result = match method {
        "initialize" => Ok(initialize_result(&params)),
        "tools/list" => Ok(json!({"tools": tool_defs()})),
        "tools/call" => Ok(tools_call_result(actor, &params)),
        "ping" => Ok(json!({})),
        _ => Err(json!({
            "code": -32601,
            "message": format!("Method not found: {method}")
        })),
    };
    let resp = match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    };
    Some(write_json(&resp))
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION);
    let version = if SUPPORTED_VERSIONS.contains(&requested) {
        requested
    } else {
        PROTOCOL_VERSION
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "winvision", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "Windows desktop control: start EVERY desktop task with desktop_snapshot (fast UIA tree, no screenshots), interact via ui_interact element_id=[N]; use visual_som_capture + ocr_read_region only for custom-rendered (DirectX/GDI-drawn) windows; confirm results with window_manager wait_for_window or a second desktop_snapshot."
    })
}

fn tools_call_result(actor: &Actor, params: &Value) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let started = Instant::now();
    let (content, is_error) = match call_tool(actor, name, &args) {
        Ok(c) => (c, false),
        Err(e) => (vec![Content::Text(e)], true),
    };
    let ms = started.elapsed().as_millis();
    let mut content_json: Vec<Value> = content.iter().map(|c| c.to_json()).collect();
    // A trailing timing hint helps the model learn tool latencies.
    if let Some(Value::Object(t)) = content_json.first_mut() {
        if let Some(Value::String(text)) = t.get_mut("text") {
            if !text.contains('(') {
                text.push_str(&format!(" ({ms} ms)"));
            }
        }
    }
    json!({"content": content_json, "isError": is_error})
}

// ---------- tool definitions ----------

pub fn tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "desktop_snapshot",
            "description": "Instant 'X-ray' of a window via Windows UI Automation (no screenshot). Returns a compact list of interactive elements with short IDs [1..N] and screen rects. IDs stay valid for 10 s. FIRST choice for any desktop task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_window": {"type": "string", "description": "window title or process name substring (default: the foreground window)"},
                    "max_depth": {"type": "integer", "description": "UIA tree depth limit (default 6)", "minimum": 1, "maximum": 15},
                    "max_elements": {"type": "integer", "description": "cap on listed elements (default 400)"}
                }
            }
        }),
        json!({
            "name": "ui_interact",
            "description": "Interact with an element from desktop_snapshot by its ID: click/double_click/right_click/focus/set_value/toggle. Tries native UIA patterns (works without moving the mouse), falls back to a hardware SendInput click.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "element_id": {"type": "integer", "description": "element ID from the last desktop_snapshot"},
                    "action": {"type": "string", "enum": ["click", "double_click", "right_click", "focus", "set_value", "toggle"]},
                    "text": {"type": "string", "description": "value for action=set_value"}
                },
                "required": ["element_id", "action"]
            }
        }),
        json!({
            "name": "visual_som_capture",
            "description": "Visual frame with Set-of-Mark annotations (numbered badges on windows and/or OCR text lines) via DXGI Desktop Duplication. Use ONLY when a window does not expose UIA elements (custom-rendered apps, games). Returns a JPEG image + coordinate table (screen pixels).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_window": {"type": "string", "description": "window title/process substring (default: whole monitor with the foreground window)"},
                    "max_width": {"type": "integer", "description": "output image width cap (default 1280)"},
                    "quality": {"type": "integer", "description": "JPEG quality 30-95 (default 80)"},
                    "annot": {"type": "string", "enum": ["windows", "ocr", "all", "none"], "description": "what to badge: top-level windows, OCR text lines, both, or none (default windows)"},
                    "grid": {"type": "boolean", "description": "draw coordinate grid (default false)"}
                }
            }
        }),
        json!({
            "name": "ocr_read_region",
            "description": "Read text from any screen region with the built-in Windows.Media.Ocr (~20-50 ms). Works on custom renderers, charts, games. Region defaults to the whole monitor under the cursor.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": {"type": "integer", "description": "screen region left (default: target window or full monitor)"},
                    "y": {"type": "integer", "description": "screen region top"},
                    "w": {"type": "integer", "description": "screen region width"},
                    "h": {"type": "integer", "description": "screen region height"},
                    "target_window": {"type": "string", "description": "OCR this window's rect instead of x/y/w/h"},
                    "language": {"type": "string", "description": "BCP-47 OCR language, e.g. 'en-US', 'ru-RU' (default: system language)"}
                }
            }
        }),
        json!({
            "name": "window_manager",
            "description": "Window/workspace management: list_windows, focus_window, minimize, maximize, restore, snap_left, snap_right, snap_full, launch_app, wait_for_window (event-less polling with OCR text match), recent_events (SetWinEventHook feed).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list_windows", "focus_window", "minimize", "maximize", "restore", "snap_left", "snap_right", "snap_full", "launch_app", "wait_for_window", "recent_events"], "description": "what to do"},
                    "target": {"type": "string", "description": "window selector: title or process substring, or hwnd (decimal or 0x hex)"},
                    "command": {"type": "string", "description": "launch_app: executable or path/URI"},
                    "args": {"type": "string", "description": "launch_app: command line arguments"},
                    "timeout_ms": {"type": "integer", "description": "wait_for_window: timeout (default 10000, cap 60000)"},
                    "poll_ms": {"type": "integer", "description": "wait_for_window: poll interval (default 200)"},
                    "wait_text": {"type": "string", "description": "wait_for_window: also wait until this text appears in the window (OCR)"},
                    "count": {"type": "integer", "description": "recent_events: how many events (default 20)"}
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "input_macro",
            "description": "Keyboard/mouse macros via SendInput: hotkey ('Ctrl+Shift+P', 'Alt+Tab'), type_text (unicode), click/move/drag/scroll at physical screen coordinates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["hotkey", "type_text", "click", "move", "drag", "scroll"]},
                    "keys": {"type": "string", "description": "hotkey: e.g. 'Ctrl+Shift+P'"},
                    "text": {"type": "string", "description": "type_text: text to type"},
                    "x": {"type": "integer", "description": "screen x (click/move/drag-from/scroll position)"},
                    "y": {"type": "integer", "description": "screen y"},
                    "to_x": {"type": "integer", "description": "drag: destination x"},
                    "to_y": {"type": "integer", "description": "drag: destination y"},
                    "button": {"type": "string", "enum": ["left", "right", "middle"], "description": "click button (default left)"},
                    "clicks": {"type": "integer", "description": "click count (default 1)"},
                    "delta": {"type": "integer", "description": "scroll amount, one notch = 120, positive = up (default -360 = 3 down)"},
                    "duration_ms": {"type": "integer", "description": "drag duration (default 300)"}
                },
                "required": ["action"]
            }
        }),
        json!({
            "name": "clipboard",
            "description": "Read or replace the clipboard text (CF_UNICODETEXT). Pairs well with input_macro hotkey 'Ctrl+v' for fast reliable text entry into custom fields.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["get", "set"]},
                    "text": {"type": "string", "description": "text for action=set"}
                },
                "required": ["action"]
            }
        }),
    ]
}

// ---------- tool dispatch ----------

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

pub fn call_tool(actor: &Actor, name: &str, args: &Value) -> Result<Vec<Content>, String> {
    let args = if args.is_null() { &Value::Object(Default::default()) } else { args };
    match name {
        "desktop_snapshot" => {
            let target = arg_str(args, "target_window");
            let depth = arg_i64(args, "max_depth").unwrap_or(6) as u32;
            let max_elements = arg_i64(args, "max_elements").unwrap_or(400) as u32;
            let text = block(actor, move |ctx| {
                ctx.uia()?.snapshot(target.as_deref(), depth, max_elements)
            })?;
            Ok(vec![Content::Text(text)])
        }
        "ui_interact" => {
            let element_id = arg_i64(args, "element_id")
                .ok_or("ui_interact requires element_id (from desktop_snapshot)")?;
            let action = arg_str(args, "action")
                .ok_or("ui_interact requires action (click|double_click|right_click|focus|set_value|toggle)")?;
            let text = arg_str(args, "text");
            let out = block(actor, move |ctx| {
                ctx.uia()?.interact(element_id as u32, &action, text.as_deref())
            })?;
            Ok(vec![Content::Text(out)])
        }
        "visual_som_capture" => {
            let target = arg_str(args, "target_window");
            let max_width = arg_i64(args, "max_width").unwrap_or(1280) as u32;
            let quality = arg_i64(args, "quality").unwrap_or(80) as u8;
            let annot = arg_str(args, "annot").unwrap_or_else(|| "windows".into());
            let grid = arg_bool(args, "grid").unwrap_or(false);
            som_capture(actor, target, max_width, quality, &annot, grid)
        }
        "ocr_read_region" => {
            let x = arg_i64(args, "x");
            let y = arg_i64(args, "y");
            let w = arg_i64(args, "w");
            let h = arg_i64(args, "h");
            let target = arg_str(args, "target_window");
            let language = arg_str(args, "language");
            let out = block(actor, move |ctx| {
                // Resolve the region in screen coordinates.
                let region: (i32, i32, i32, i32) = if let Some(t) = &target {
                    let win = window::find_window(t)
                        .ok_or_else(|| format!("no window matches '{t}'"))?;
                    (win.x, win.y, win.w, win.h)
                } else if let (Some(x), Some(y), Some(w), Some(h)) = (x, y, w, h) {
                    (x as i32, y as i32, w as i32, h as i32)
                } else {
                    (0, 0, 0, 0) // whole monitor under the cursor
                };
                let frame = crate::dxgi::with(|dx| dx.capture_for_point(region_center(&region)))?;
                let (x0, y0, cw, ch) = if region.2 > 0 && region.3 > 0 {
                    (
                        (region.0 - frame.origin.0).max(0),
                        (region.1 - frame.origin.1).max(0),
                        (region.2 as u32).min(frame.w.saturating_sub(region.0.max(0) as u32)),
                        (region.3 as u32).min(frame.h.saturating_sub(region.1.max(0) as u32)),
                    )
                } else {
                    (0, 0, frame.w, frame.h)
                };
                if cw == 0 || ch == 0 {
                    return Err("region is empty on screen".into());
                }
                let (crop, cw, ch) = crate::dxgi::crop_bgra(&frame.bgra, frame.w, frame.h, x0, y0, cw, ch);
                // Upscale small regions for better OCR accuracy.
                let (buf, ow, oh) = if cw < 240 {
                    let img = crate::dxgi::bgra_to_rgb(&crop, cw, ch);
                    let up = image::imageops::resize(
                        &img, (cw * 2).min(frame.w), (ch * 2).min(frame.h), image::imageops::FilterType::Triangle,
                    );
                    let mut bgra = Vec::with_capacity((up.width() * up.height() * 4) as usize);
                    for p in up.pixels() {
                        bgra.extend_from_slice(&[p.0[2], p.0[1], p.0[0], 255]);
                    }
                    (bgra, up.width(), up.height())
                } else {
                    (crop, cw, ch)
                };
                let scale = ow as f32 / cw as f32;
                let out = ctx.ocr()?.recognize(&buf, ow, oh, language.as_deref())?;
                let mut s = String::with_capacity(1024);
                s.push_str(&format!(
                    "OCR {}x{} region at ({},{}) screen — language {}:\n",
                    cw, ch, region.0.max(frame.origin.0), region.1.max(frame.origin.1), out.language
                ));
                for (i, l) in out.lines.iter().enumerate() {
                    let (lx, ly, lw, lh) = l.rect;
                    let sx = frame.origin.0 + x0 + (lx as f32 / scale).round() as i32;
                    let sy = frame.origin.1 + y0 + (ly as f32 / scale).round() as i32;
                    s.push_str(&format!(
                        "[{}] \"{}\" (x: {}, y: {}, w: {}, h: {})\n",
                        i + 1,
                        l.text,
                        sx,
                        sy,
                        (lw as f32 / scale).round() as i32,
                        (lh as f32 / scale).round() as i32
                    ));
                }
                s.push_str(&format!("\nTEXT:\n{}", out.text));
                Ok(s)
            })?;
            Ok(vec![Content::Text(out)])
        }
        "window_manager" => window_manager_tool(actor, args),
        "input_macro" => input_macro_tool(actor, args),
        "clipboard" => {
            let action = arg_str(args, "action").ok_or("clipboard requires action (get|set)")?;
            let text = arg_str(args, "text");
            let out = block(actor, move |_| match action.as_str() {
                "get" => crate::clipboard::get_text(),
                "set" => crate::clipboard::set_text(
                    text.as_deref().ok_or("clipboard set requires text")?,
                ),
                _ => Err("unknown clipboard action (use get|set)".into()),
            })?;
            Ok(vec![Content::Text(out)])
        }
        _ => Err(format!(
            "unknown tool '{name}' — available: desktop_snapshot, ui_interact, visual_som_capture, ocr_read_region, window_manager, input_macro, clipboard"
        )),
    }
}

fn region_center(r: &(i32, i32, i32, i32)) -> (i32, i32) {
    if r.2 > 0 && r.3 > 0 {
        (r.0 + r.2 / 2, r.1 + r.3 / 2)
    } else {
        cursor_point()
    }
}

fn cursor_point() -> (i32, i32) {
    unsafe {
        let mut p = windows::Win32::Foundation::POINT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
        (p.x, p.y)
    }
}

fn block<R, F>(actor: &Actor, f: F) -> Result<R, String>
where
    R: Send + 'static,
    F: FnOnce(&mut crate::actor::Ctx) -> Result<R, String> + Send + 'static,
{
    // Inside a tokio worker use block_in_place; in sync contexts (tests)
    // just call directly.
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| actor.call(f))
    } else {
        actor.call(f)
    }
}

// ---------- visual_som_capture ----------

#[allow(clippy::too_many_arguments)]
fn som_capture(
    actor: &Actor,
    target: Option<String>,
    max_width: u32,
    quality: u8,
    annot: &str,
    grid: bool,
) -> Result<Vec<Content>, String> {
    let annot = annot.to_string();
    let (table, jpeg) = block(actor, move |ctx| {
        let started = Instant::now();
        // Resolve target / capture region.
        let (center, crop) = match &target {
            Some(t) => {
                let win = window::find_window(t)
                    .ok_or_else(|| format!("no window matches '{t}'"))?;
                ((win.x + win.w / 2, win.y + win.h / 2), Some(win.rect()))
            }
            None => {
                let c = cursor_point();
                (c, None)
            }
        };
        let frame = crate::dxgi::with(|dx| dx.capture_for_point(center))?;
        let (x0, y0, cw, ch) = match crop {
            Some(r) => {
                let x0 = (r.left - frame.origin.0).max(0);
                let y0 = (r.top - frame.origin.1).max(0);
                let cw = ((r.right - r.left) as u32).min(frame.w.saturating_sub(x0 as u32)).max(1);
                let ch = ((r.bottom - r.top) as u32).min(frame.h.saturating_sub(y0 as u32)).max(1);
                (x0, y0, cw, ch)
            }
            None => (0, 0, frame.w, frame.h),
        };
        let (crop_data, cw, ch) = crate::dxgi::crop_bgra(&frame.bgra, frame.w, frame.h, x0, y0, cw, ch);
        let crop_origin = (frame.origin.0 + x0, frame.origin.1 + y0);
        let rgb = crate::dxgi::bgra_to_rgb(&crop_data, cw, ch);
        let scaled = crate::som::scale_to_width(&rgb, max_width);
        let scale = scaled.width() as f32 / cw as f32;
        let mut img = scaled;

        let mut table = String::with_capacity(2048);
        let mut next_badge = 1u32;
        let mut badges = Vec::new();
        let mut rows = Vec::new();

        if annot == "windows" || annot == "all" {
            let mut wins: Vec<window::WindowInfo> = window::collect_windows(false)
                .into_iter()
                .filter(|w| {
                    let r = w.rect();
                    r.left < crop_origin.0 + cw as i32
                        && r.right > crop_origin.0
                        && r.top < crop_origin.1 + ch as i32
                        && r.bottom > crop_origin.1
                })
                .collect();
            wins.sort_by_key(|w| std::cmp::Reverse(w.w * w.h));
            for w in wins.iter().take(30) {
                let bx = (((w.x - crop_origin.0) as f32 * scale).round() as i32).max(0);
                let by = (((w.y - crop_origin.1) as f32 * scale).round() as i32).max(0);
                badges.push(crate::som::Badge { n: next_badge, x: bx, y: by });
                rows.push(format!(
                    "[{}] window \"{}\" ({}) (x: {}, y: {}, w: {}, h: {})",
                    next_badge, w.title, w.process, w.x, w.y, w.w, w.h
                ));
                next_badge += 1;
            }
        }
        if annot == "ocr" || annot == "all" {
            let out = ctx
                .ocr()?
                .recognize(&crop_data, cw, ch, None)
                .map_err(|e| format!("OCR failed: {e}"))?;
            for l in out.lines.iter().take(40) {
                if l.text.trim().is_empty() {
                    continue;
                }
                let (lx, ly, lw, lh) = l.rect;
                let bx = ((lx as f32 * scale).round() as i32).max(0);
                let by = ((ly as f32 * scale).round() as i32).max(0);
                badges.push(crate::som::Badge { n: next_badge, x: bx, y: by });
                let sx = crop_origin.0 + lx;
                let sy = crop_origin.1 + ly;
                rows.push(format!(
                    "[{}] text \"{}\" (x: {}, y: {}, w: {}, h: {})",
                    next_badge, l.text, sx, sy, lw, lh
                ));
                next_badge += 1;
            }
        }

        if grid {
            crate::som::draw_grid(&mut img, 120);
        }
        for b in &badges {
            crate::som::draw_badge(&mut img, b);
        }
        let jpeg = crate::som::encode_jpeg(&img, quality)?;
        let ms = started.elapsed().as_millis();
        table.push_str(&format!(
            "SoM frame: {}x{} image of {}x{} region at screen ({},{}), {} badges, {} ms\n{}\n\nTo click mark [N] use input_macro click with its coordinates; to read more text use ocr_read_region.",
            img.width(),
            img.height(),
            cw,
            ch,
            crop_origin.0,
            crop_origin.1,
            badges.len(),
            ms,
            rows.join("\n")
        ));
        Ok((table, jpeg))
    })?;
    Ok(vec![
        Content::Text(table),
        Content::Image {
            data: base64_encode(&jpeg),
            mime: "image/jpeg",
        },
    ])
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

// ---------- window_manager ----------

fn window_manager_tool(actor: &Actor, args: &Value) -> Result<Vec<Content>, String> {
    let action = arg_str(args, "action").ok_or("window_manager requires action")?;
    let target = arg_str(args, "target");
    match action.as_str() {
        "list_windows" => {
            let out = block(actor, move |_| {
                let wins = window::collect_windows(false);
                let mut s = format!("{} top-level windows (Z order):\n", wins.len());
                for w in &wins {
                    s.push_str(&format!("  {}\n", w.line()));
                }
                Ok(s)
            })?;
            Ok(vec![Content::Text(out)])
        }
        "focus_window" | "minimize" | "maximize" | "restore" | "snap_left" | "snap_right"
        | "snap_full" => {
            let target = target.ok_or_else(|| format!("{action} requires 'target'"))?;
            let out = block(actor, move |_| {
                let win = window::find_window(&target)
                    .ok_or_else(|| format!("no window matches '{target}'"))?;
                match action.as_str() {
                    "focus_window" => window::focus_window(win.hwnd),
                    "minimize" | "maximize" | "restore" => {
                        window::set_window_state(win.hwnd, &action)
                    }
                    other => window::snap_window(
                        win.hwnd,
                        other.trim_start_matches("snap_"), // left | right | full
                    ),
                }
            })?;
            Ok(vec![Content::Text(out)])
        }
        "launch_app" => {
            let command = arg_str(args, "command").ok_or("launch_app requires 'command'")?;
            let launch_args = arg_str(args, "args").unwrap_or_default();
            let out = block(actor, move |_| window::launch_app(&command, &launch_args))?;
            Ok(vec![Content::Text(out)])
        }
        "wait_for_window" => {
            // Polling loop: each probe is a small actor job so the COM
            // thread stays responsive while we wait.
            let target = target.clone();
            let wait_text = arg_str(args, "wait_text");
            let timeout_ms = arg_i64(args, "timeout_ms").unwrap_or(10_000).clamp(100, 60_000) as u64;
            let poll_ms = arg_i64(args, "poll_ms").unwrap_or(200).clamp(50, 2000) as u64;
            let deadline = Instant::now() + std::time::Duration::from_millis(timeout_ms);
            loop {
                let t = target.clone();
                let found = block(actor, move |_| Ok(window::find_window(&t.unwrap_or_default())))?;
                let mut matched = found.clone();
                if let (Some(win), Some(wt)) = (&found, &wait_text) {
                    // Text check: OCR the window rect.
                    let rect = win.rect();
                    let wt = wt.clone();
                    let ok = block(actor, move |ctx| {
                        let frame = crate::dxgi::with(|dx| {
                            dx.capture_for_point((
                                (rect.left + rect.right) / 2,
                                (rect.top + rect.bottom) / 2,
                            ))
                        })?;
                        let (crop, cw, ch) = crate::dxgi::crop_bgra(
                            &frame.bgra,
                            frame.w,
                            frame.h,
                            (rect.left - frame.origin.0).max(0),
                            (rect.top - frame.origin.1).max(0),
                            (rect.right - rect.left).min(frame.w as i32).max(1) as u32,
                            (rect.bottom - rect.top).min(frame.h as i32).max(1) as u32,
                        );
                        let out = ctx.ocr()?.recognize(&crop, cw, ch, None)?;
                        Ok(out.text.to_lowercase().contains(&wt.to_lowercase()))
                    })?;
                    if ok {
                        matched = found.clone();
                    } else {
                        matched = None;
                    }
                }
                if let Some(win) = matched {
                    let extra = wait_text
                        .as_ref()
                        .map(|_| " (text confirmed via OCR)")
                        .unwrap_or("");
                    return Ok(vec![Content::Text(format!(
                        "window appeared{extra}: {}",
                        win.line()
                    ))]);
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "wait_for_window timed out after {timeout_ms} ms (target: '{}'{})",
                        target.as_deref().unwrap_or("(any)"),
                        wait_text
                            .as_ref()
                            .map(|t| format!(", text: '{t}'"))
                            .unwrap_or_default()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
            }
        }
        "recent_events" => {
            let count = arg_i64(args, "count").unwrap_or(20).clamp(1, 128) as usize;
            let out = block(actor, move |ctx| {
                let events = ctx.event_state.recent(count);
                let mut s = format!("{} recent window events (newest first):\n", events.len());
                for e in events {
                    let ago = e.at.elapsed().as_millis();
                    s.push_str(&format!(
                        "  {}ms ago {} hwnd 0x{:x} \"{}\"\n",
                        ago, e.kind, e.hwnd, e.title
                    ));
                }
                Ok(s)
            })?;
            Ok(vec![Content::Text(out)])
        }
        _ => Err(format!("unknown window_manager action '{action}'")),
    }
}

// ---------- input_macro ----------

fn input_macro_tool(actor: &Actor, args: &Value) -> Result<Vec<Content>, String> {
    let action = arg_str(args, "action").ok_or("input_macro requires action")?;
    // Extract all params up front — the closure must own its data.
    let keys = arg_str(args, "keys");
    let text = arg_str(args, "text");
    let x = arg_i64(args, "x").map(|v| v as i32);
    let y = arg_i64(args, "y").map(|v| v as i32);
    let to_x = arg_i64(args, "to_x").map(|v| v as i32);
    let to_y = arg_i64(args, "to_y").map(|v| v as i32);
    let button = arg_str(args, "button").unwrap_or_else(|| "left".into());
    let clicks = arg_i64(args, "clicks").unwrap_or(1).max(1) as u32;
    let delta = arg_i64(args, "delta").unwrap_or(-360) as i32;
    let duration_ms = arg_i64(args, "duration_ms").unwrap_or(300).clamp(60, 5000) as u32;
    let out = block(actor, move |_| match action.as_str() {
        "hotkey" => {
            let keys = keys.ok_or("hotkey requires 'keys', e.g. 'Ctrl+Shift+P'")?;
            crate::input::send_hotkey(&keys)
        }
        "type_text" => {
            let text = text.ok_or("type_text requires 'text'")?;
            crate::input::type_text(&text)
        }
        "click" => {
            let x = x.ok_or("click requires x,y")?;
            let y = y.ok_or("click requires x,y")?;
            crate::input::click_at(x, y, &button, clicks)
        }
        "move" => {
            let x = x.ok_or("move requires x,y")?;
            let y = y.ok_or("move requires x,y")?;
            crate::input::move_mouse(x, y, true)?;
            Ok(format!("cursor moved to ({x},{y})"))
        }
        "drag" => {
            let x1 = x.ok_or("drag requires x,y (from)")?;
            let y1 = y.ok_or("drag requires x,y (from)")?;
            let x2 = to_x.ok_or("drag requires to_x,to_y")?;
            let y2 = to_y.ok_or("drag requires to_y")?;
            crate::input::drag(x1, y1, x2, y2, duration_ms)
        }
        "scroll" => crate::input::scroll(x, y, delta),
        _ => Err("unknown input_macro action (use hotkey|type_text|click|move|drag|scroll)".into()),
    })?;
    Ok(vec![Content::Text(out)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_echoes_supported_version() {
        let r = initialize_result(&json!({"protocolVersion": "2025-03-26"}));
        assert_eq!(r["protocolVersion"], "2025-03-26");
        assert_eq!(r["serverInfo"]["name"], "winvision");
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_falls_back_on_unknown_version() {
        let r = initialize_result(&json!({"protocolVersion": "1999-01-01"}));
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn tools_list_has_seven_valid_schemas() {
        let tools = tool_defs();
        assert_eq!(tools.len(), 7);
        for t in &tools {
            assert!(t["name"].is_string(), "tool missing name");
            assert_eq!(t["inputSchema"]["type"], "object", "schema must be an object schema");
            assert!(t["description"].as_str().unwrap_or("").len() > 10);
        }
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in [
            "desktop_snapshot",
            "ui_interact",
            "visual_som_capture",
            "ocr_read_region",
            "window_manager",
            "input_macro",
            "clipboard",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    #[test]
    fn unknown_method_is_32601() {
        let actor = crate::actor::Actor::spawn();
        let resp = handle_line(&actor, r#"{"jsonrpc":"2.0","id":9,"method":"bogus/x"}"#).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32601);
        assert_eq!(v["id"], 9);
    }

    #[test]
    fn parse_error_is_32700() {
        let actor = crate::actor::Actor::spawn();
        let resp = handle_line(&actor, "{not json").unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[test]
    fn notifications_produce_no_response() {
        let actor = crate::actor::Actor::spawn();
        assert!(handle_line(
            &actor,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
    }

    #[test]
    fn ping_pong() {
        let actor = crate::actor::Actor::spawn();
        let resp = handle_line(&actor, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"], json!({}));
        assert!(resp.lines().count() == 1, "response must be single-line");
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let actor = crate::actor::Actor::spawn();
        let resp = handle_line(
            &actor,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
        assert!(v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn missing_required_args_is_error_result() {
        let actor = crate::actor::Actor::spawn();
        let resp = handle_line(
            &actor,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ui_interact","arguments":{}}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }

    #[test]
    fn tool_result_is_single_line_json() {
        let actor = crate::actor::Actor::spawn();
        let resp = handle_line(&actor, r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"window_manager","arguments":{"action":"list_windows"}}}"#).unwrap();
        assert!(!resp.contains('\n'), "MCP stdio forbids embedded newlines");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["content"].is_array());
    }

    #[test]
    fn image_content_shape() {
        let c = Content::Image { data: "abc".into(), mime: "image/jpeg" };
        let j = c.to_json();
        assert_eq!(j["type"], "image");
        assert_eq!(j["mimeType"], "image/jpeg");
    }
}
