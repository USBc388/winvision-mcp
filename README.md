# winvision-mcp

Local MCP server (Model Context Protocol, stdio JSON-RPC 2.0) for Windows 10/11, written in pure Rust. Exposes Windows UI Automation trees, hardware screen capture via DXGI Desktop Duplication (automatic GDI BitBlt fallback), offline OCR via Windows.Media.Ocr, and Win32 `SendInput` emulation. Ships as a single release binary (~975 KB with strip + LTO), zero external runtime dependencies, zero network calls.

Works with any MCP client that launches a stdio server: Claude Code, Claude Desktop, Cursor, OpenAI Codex CLI, Windsurf, Cline, Roo Code, ZCode CLI, and others. Configuration snippets: [Client configuration](#client-configuration) and [examples/configs/](examples/configs/).

## Tools

| Tool | Function | p50 / p95 |
|---|---|---|
| `desktop_snapshot` | UIA tree of a window as an ID-tagged element list with screen rects. Browser accessibility trees are woken automatically; walk depth 14 for browser windows | 34 ms / 48 ms |
| `ui_interact` | click / double_click / right_click / focus / set_value / toggle by element ID; native UIA patterns first, hardware `SendInput` fallback | 0.3 ms / 1.1 ms |
| `ocr_click_text` | capture → OCR → word-level bounding box of the query → `SendInput` click / double_click / type_after_click (clipboard paste) | 44 ms / 393 ms* |
| `visual_som_capture` | DXGI frame with numbered Set-of-Mark badges on windows/OCR lines + coordinate table | 72 ms / 421 ms* |
| `ocr_read_region` | offline OCR of a screen region, per-line boxes in screen coordinates | 44 ms / 393 ms* |
| `window_manager` | list_windows, focus, minimize/maximize/restore, snap_left/right/full, launch_app, wait_for_window, recent_events | 1.4 ms / 3.8 ms |
| `input_macro` | hotkey, type_text (Unicode), click, move, drag, scroll | ms-range |
| `system_macro` | replayable step sequences (focus / click / type / hotkey / scroll / drag / wait / launch / ocr_click / clipboard_set) in one call; named macros persist on disk | ms per step |
| `clipboard` | get/set `CF_UNICODETEXT` | 1.1 ms / 1.2 ms |

Latencies: full JSON-RPC round trip on a live desktop (Core i5-10400, GTX 1660 SUPER, 2560×1440, Windows 11 build 26300). Methodology and hardware details: [BENCHMARKS.md](BENCHMARKS.md).
\* Capture-tool p95 includes the automatic GDI fallback path described under [Architecture notes](#architecture-notes).

## Architecture notes

- Dedicated MTA COM actor thread with a Win32 message pump (`actor.rs`); all UIA, DXGI and OCR state is owned by that thread, the async stdio loop never touches COM directly.
- Single-roundtrip batched UIA tree caching: `BuildUpdatedCache` with `TreeScope=Subtree` fetches an entire window in one cross-process call; element handles are kept in a 10 s TTL cache for `ui_interact`.
- Automatic browser accessibility wake-up (Chrome, Edge, Firefox, Zen, Brave, Floorp, LibreWolf, Opera, Vivaldi): `WM_GETOBJECT` with `OBJID_CLIENT` via `SendMessageTimeoutW` plus a temporary `SPI_SETSCREENREADER` flag, restored after the scan.
- Process-wide DXGI Output Duplication singleton with automatic session recovery on `DXGI_ERROR_ACCESS_LOST` and immediate GDI BitBlt fallback when DWM pauses composition (fullscreen apps, animated-wallpaper software).
- Event-driven window state tracking via `SetWinEventHook` into a bounded ring buffer.
- Input emulation via `SendInput` (scan codes + `KEYEVENTF_UNICODE`), mouse coordinates normalized across the virtual desktop.

## Build from source

Requires a Rust toolchain (edition 2021) and Windows 10/11.

```powershell
git clone https://github.com/USBc388/winvision-mcp.git
cd winvision-mcp
cargo build --release
```

Output binary: `target\release\winvision-mcp.exe`.

## Binary verification

Released binaries are hashed with SHA256; the digest is recorded in [SHA256SUMS.txt](SHA256SUMS.txt) and in the release notes.

```powershell
Get-FileHash .\winvision-mcp.exe -Algorithm SHA256
```

## Client configuration

Any MCP client that launches a stdio server works. Standalone files: [examples/configs/](examples/configs/). Replace `C:\tools\winvision-mcp.exe` with the actual binary path.

<details>
<summary>Claude Code</summary>

`~/.claude.json` (or project-level `.mcp.json`):

```json
{"mcpServers":{"winvision":{"type":"stdio","command":"C:\\tools\\winvision-mcp.exe","args":[]}}}
```

CLI alternative:

```powershell
claude mcp add winvision -- C:\tools\winvision-mcp.exe
```

</details>

<details>
<summary>Cursor</summary>

`~/.cursor/mcp.json`:

```json
{"mcpServers":{"winvision":{"command":"C:\\tools\\winvision-mcp.exe","args":[]}}}
```

</details>

<details>
<summary>OpenAI Codex CLI</summary>

`~/.codex/config.toml`:

```toml
[mcp_servers.winvision]
command = "C:\\tools\\winvision-mcp.exe"
args = []
```

</details>

<details>
<summary>Windsurf</summary>

`~/.codeium/windsurf/mcp_config.json`:

```json
{"mcpServers":{"winvision":{"command":"C:\\tools\\winvision-mcp.exe","args":[]}}}
```

</details>

<details>
<summary>Cline / Roo Code</summary>

`cline_mcp_settings.json` (Roo Code uses the same format):

```json
{"mcpServers":{"winvision":{"command":"C:\\tools\\winvision-mcp.exe","args":[],"alwaysAllow":["desktop_snapshot","ui_interact","ocr_click_text","visual_som_capture","ocr_read_region","window_manager","input_macro","system_macro","clipboard"]}}}
```

</details>

<details>
<summary>Claude Desktop</summary>

`%APPDATA%\Claude\claude_desktop_config.json`:

```json
{"mcpServers":{"winvision":{"command":"C:\\tools\\winvision-mcp.exe","args":[]}}}
```

</details>

<details>
<summary>ZCode CLI</summary>

`~/.zcode/cli/config.json`:

```json
{"mcp":{"servers":{"winvision":{"command":"C:\\tools\\winvision-mcp.exe","args":[]}}}}
```

</details>

## Tests

```powershell
cargo clippy --all-targets
cargo test                # 49 unit tests
cargo test -- --ignored   # 11 integration tests (real UIA/DXGI/OCR; interactive desktop required)
```

## Known limitations

- The first UIA call in a process pays COM initialization (~300–700 ms); warm snapshots of typical windows take 10–50 ms. The explorer desktop-shell window answers in ~650 ms (provider-side cost).
- Browser pages build their UIA trees lazily; the server wakes them per snapshot, but heavily loaded pages may still need a retry.
- OCR requires a Windows language pack matching the on-screen text.
- A fully static desktop can stop NVIDIA drivers from producing duplication frames; the server falls back to GDI capture automatically.
