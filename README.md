# WinVision MCP

An ultra-fast local MCP server (Model Context Protocol, stdio JSON-RPC 2.0)
for ZCode: native Windows "vision" and desktop control in pure Rust. It
replaces the slow screenshot → pixel-guessing → click loop with instant
access to the UI Automation tree, hardware-level DXGI screen capture and
event-driven window monitoring.

**Speed (p50, full JSON-RPC round trip, live desktop):** warm window
snapshot — **34 ms**, button click via a UIA pattern — **0.3 ms**, screen
region OCR — **44 ms**, capture + Set-of-Mark overlay — **72 ms**.
Full benchmarks: [BENCHMARKS.md](BENCHMARKS.md). Prebuilt exe in
[Releases](../../releases).

## Tools

| Tool | Purpose | Speed |
|---|---|---|
| `desktop_snapshot` | Instant "X-ray" of a window via UI Automation: interactive elements with short IDs `[N]` and screen rects. Browsers (Chrome/Edge/Firefox/Zen/Floorp/Brave) are woken automatically (WM_GETOBJECT + screen-reader flag) with walk depth 14 | warm ~10–50 ms |
| `ui_interact` | click / double_click / right_click / focus / set_value / toggle by `element_id`; tries native patterns first (Invoke/Value/Toggle/SelectionItem/ExpandCollapse), falls back to a hardware SendInput click | ~0.3–5 ms |
| `ocr_click_text` | Click exactly on text on screen: capture (DXGI/GDI) → OCR → bounding box of the found text → SendInput click at its center; `type_after_click` pastes text via Ctrl+V. Works on sites with no UIA tree, Canvas, captchas | ~50–150 ms |
| `visual_som_capture` | JPEG frame via DXGI Desktop Duplication with Set-of-Mark badges on windows/OCR lines + a coordinate table | < 5 ms capture |
| `ocr_read_region` | Offline OCR of any region (Windows.Media.Ocr), per-line boxes in screen coordinates | ~20–50 ms |
| `window_manager` | list_windows, focus, minimize/maximize/restore, snap_left/right/full, launch_app, wait_for_window (with OCR text check), recent_events (SetWinEventHook) | ms |
| `input_macro` | hotkey (`Ctrl+Shift+P`), type_text (Unicode), click/move/drag/scroll | ms |
| `system_macro` | **v0.2.1**: replayable macros — a sequence of steps (focus window → click → type → hotkey → scroll → drag → wait → launch app → OCR-click → clipboard) executed in ONE call with inter-step delays; named macros persist to `%USERPROFILE%\.winvision-mcp\macros` (save/load/list/delete) | ms per step |
| `clipboard` | get/set CF_UNICODETEXT | ms |

## Architecture

```
src/
  main.rs      — startup: DPI-aware, COM actor, tokio stdio
  mcp.rs       — MCP transport: initialize/tools/list/tools/call, schemas for 8 tools
  actor.rs     — dedicated COM thread (CoInitializeEx MTA + message pump), all Win32/COM calls
  uia.rs       — UIA: batched tree cache (BuildUpdatedCache, TreeScope=Subtree — 1 round trip),
                 10 s ID→element cache, patterns + fallback, browser accessibility auto-wake
  dxgi.rs      — DXGI Desktop Duplication: one duplication per process (global singleton),
                 survives ACCESS_LOST / mode changes / monitor switches, GDI fallback
  som.rs       — Set-of-Mark: numbered color badges (built-in 5x7 font), grid, JPEG
  ocr.rs       — Windows.Media.Ocr: engine creation with fallbacks, word/line boxes
  window.rs    — EnumWindows/ShowWindow/snap/launch (foreground-lock workaround via Alt tap)
  events.rs    — SetWinEventHook (foreground/show/destroy/namechange) into a ring buffer
  input.rs     — SendInput: hotkeys, Unicode typing, interpolated drag, wheel
  clipboard.rs — CF_UNICODETEXT
```

## Building

```powershell
cargo build --release     # target/release/winvision-mcp.exe
cargo clippy --all-targets
cargo test                # 43 unit tests
cargo test -- --ignored   # 10 integration tests (real UIA/DXGI/OCR/stdio, needs an interactive desktop)
```

## Connecting to ZCode

In `~/.zcode/cli/config.json`:
```json
"mcp": { "servers": { "winvision": {
  "command": "C:\\tools\\winvision-mcp.exe",
  "args": []
} } }
```

A skill file describing usage rules lives in
`.zcode/skills/winvision/SKILL.md` (per-project) and
`~/.zcode/skills/winvision/SKILL.md` (global).

Manual smoke check:
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\smoke-release.ps1
```

## Notes

- The first UIA call in a process pays cold COM initialization (~300–700 ms);
  warm snapshots of typical windows take 10–50 ms. The desktop shell window
  (`Program Manager`) answers its provider in ~650 ms — an explorer-side
  limitation, not a server one.
- Browsers wake their accessibility tree on demand; the server sends the
  wake probe automatically on every browser snapshot (v0.2.0+).
- Fully static screens (e.g. an animated-wallpaper app paused behind a
  focused window can stop NVIDIA from delivering duplication frames) fall
  back to a GDI BitBlt capture automatically.
- OCR requires a Windows language pack (en-US / ru-RU, etc.).
