# WinVision benchmarks

Per-call latency of every MCP tool, measured end-to-end over stdio
(full JSON-RPC round trip: client writes a request, reads the response),
against the release binary, on a live desktop.

**Machine:** Intel Core i5-10400 @ 2.90 GHz · NVIDIA GeForce GTX 1660 SUPER · 16 GB RAM ·
Windows 11 Pro (10.0.26300) · 2560×1440 @ 100% DPI
**Binary:** release profile (LTO, codegen-units=1, strip), rustc 1.100.0
**Method:** `scripts/bench.ps1` — spawns the server, warms up, then takes 10–15
samples per tool with 20 ms gaps. Target window: Notepad (39 UIA elements).

| Tool | Samples | min | p50 | p95 | max |
|---|---|---|---|---|---|
| `desktop_snapshot` — **cold** (first call in process, COM init) | 1 | — | 386 ms | — | — |
| `desktop_snapshot` — **warm** (Notepad) | 15 | 32.3 | **34.0 ms** | 47.7 | 48.2 |
| `ui_interact` focus (UIA SetFocus) | 10 | 0.28 | **0.32 ms** | 1.10 | 1.23 |
| `ui_interact` set_value (UIA ValuePattern) | 5 | 1.5 | **1.8 ms** | 5.2 | 9.0 |
| `ocr_read_region` (DXGI grab + Windows.Media.Ocr) | 10 | 40.6 | **43.7 ms** | 393* | 414 |
| `visual_som_capture` (grab + JPEG + SoM badges) | 10 | 70.3 | **72.3 ms** | 421* | 425 |
| `window_manager` list_windows (full EnumWindows scan) | 10 | 0.76 | **1.4 ms** | 3.8 | 3.9 |
| `clipboard` get | 10 | 0.26 | **1.1 ms** | 1.2 | 1.2 |

*\* p95 on capture tools includes the automatic GDI-fallback path (see below)
which triggers when the desktop is fully static.*

## Why the numbers look like this

- **Cold start (~300–400 ms)** happens once per server process: COM
  initialization, UIA client bootstrap and the first provider round trip.
  ZCode keeps the server alive for the whole session, so warm calls are what
  matter.
- **Warm snapshot ≈ 30–35 ms** even though the UIA tree walk is one batched
  cross-process call (`BuildUpdatedCache` with `TreeScope=Subtree`): the
  remaining cost is the provider serializing the subtree. Bigger windows
  (hundreds of elements) scale linearly.
- **Pattern interactions are ~free** (0.3–2 ms): `InvokePattern` /
  `ValuePattern` never move the mouse — they act on the provider directly.
- **Capture tools** read the frame straight from the GPU compositor
  (DXGI Desktop Duplication). p95 cases hit the automatic fallback chain:
  when the desktop is fully static (an animated-wallpaper app pauses behind a
  focused window, NVIDIA driver delivers no duplication frames), the server
  retries once and then falls back to a GDI BitBlt capture — slower, but it
  never stalls.

## Reproduce

```powershell
cargo build --release
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bench.ps1
# writes bench-results.md
```

Test suite also asserts the snapshot budget in CI-less mode:
`cargo test -- --ignored` (median warm snapshot must be ≤ 50 ms).
