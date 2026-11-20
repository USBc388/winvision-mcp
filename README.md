# WinVision MCP

Сверхбыстрый локальный MCP-сервер (Model Context Protocol, stdio JSON-RPC 2.0)
для ZCode: нативное «зрение» и управление рабочим столом Windows на чистом
Rust. Заменяет медленный цикл «скриншот → угадывание пикселей → клик» на
мгновенный доступ к дереву UI Automation, аппаратный захват экрана DXGI и
событийный мониторинг окон.

**Скорость (p50, полная JSON-RPC round trip, живой десктоп):**
тёплый снапшот окна — **34 мс**, нажатие кнопки через UIA-паттерн — **0.3 мс**,
OCR области экрана — **44 мс**, захват+SoM-разметка — **72 мс**.
Полные бенчмарки: [BENCHMARKS.md](BENCHMARKS.md). Готовый exe — в
[Releases](../../releases).

## Инструменты

| Tool | Назначение | Скорость |
|---|---|---|
| `desktop_snapshot` | «Рентген» окна через UI Automation: список интерактивных элементов с ID `[N]` и координатами. Браузеры (Chrome/Edge/Firefox/Zen/Floorp/Brave) будятся автоматически (WM_GETOBJECT + screen-reader flag), глубина обхода 14 | тёплый ~10–50 мс |
| `ui_interact` | click / double_click / right_click / focus / set_value / toggle по `element_id`; сначала нативные паттерны (Invoke/Value/Toggle/SelectionItem/ExpandCollapse), fallback — аппаратный SendInput-клик | ~0.3–5 мс |
| `ocr_click_text` | Клик точно по тексту на экране: захват (DXGI/GDI) → OCR → bounding box найденного текста → SendInput-клик в центр; `type_after_click` вставляет текст через Ctrl+V. Работает на сайтах без UIA-дерева, Canvas, капчах | ~50–150 мс |
| `visual_som_capture` | JPEG-кадр через DXGI Desktop Duplication с бейджами Set-of-Mark на окнах/OCR-строках + таблица координат | < 5 мс захват |
| `ocr_read_region` | Оффлайн-OCR региона (Windows.Media.Ocr), построчные боксы в экранных координатах | ~20–50 мс |
| `window_manager` | list_windows, focus, minimize/maximize/restore, snap_left/right/full, launch_app, wait_for_window (с OCR-проверкой текста), recent_events (SetWinEventHook) | мс |
| `input_macro` | hotkey (`Ctrl+Shift+P`), type_text (Unicode), click/move/drag/scroll | мс |
| `clipboard` | get/set CF_UNICODETEXT | мс |

## Архитектура

```
src/
  main.rs    — запуск: DPI-aware, COM-actor, tokio-stdio
  mcp.rs     — MCP-транспорт: initialize/tools/list/tools/call, схемы 7 инструментов
  actor.rs   — выделенный COM-поток (CoInitializeEx MTA + message pump), все Win32/COM вызовы
  uia.rs     — UIA: батч-кэш дерева (BuildUpdatedCache, TreeScope=Subtree — 1 round-trip),
               кэш ID→элемент на 10 с, паттерны + fallback
  dxgi.rs    — DXGI Desktop Duplication: 1 дублирование на процесс (глобальный singleton),
               переживает ACCESS_LOST/смену режима/монитора
  som.rs     — Set-of-Mark: цветные бейджи с номерами (встроенный шрифт 5x7), сетка, JPEG
  ocr.rs     — Windows.Media.Ocr: создание движка с фолбэками, построчные боксы
  window.rs  — EnumWindows/ShowWindow/snap/launch (обход foreground-lock через Alt-тап)
  events.rs  — SetWinEventHook (foreground/show/destroy/namechange) в ring-buffer
  input.rs   — SendInput: хоткеи, Unicode-печать, интерполированный drag, колесо
  clipboard.rs — CF_UNICODETEXT
```

## Сборка

```powershell
cargo build --release     # target/release/winvision-mcp.exe
cargo clippy --all-targets
cargo test                # 39 unit-тестов
cargo test -- --ignored   # 8 интеграционных (реальные UIA/DXGI/OCR/stdio, нужен интерактивный десктоп)
```

## Подключение к ZCode

`~/.zcode/cli/config.json`:
```json
"mcp": { "servers": { "winvision": {
  "command": "F:\\skill creation\\winvision-mcp\\target\\release\\winvision-mcp.exe",
  "args": []
} } }
```

Скилл с правилами работы: `.zcode/skills/winvision/SKILL.md` (проект) и
`~/.zcode/skills/winvision/SKILL.md` (глобально).

Проверка вручную:
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\smoke-release.ps1
```

## Замечания

- Первый вызов UIA в процессе — холодная инициализация COM (~300–700 мс);
  тёплые снапшоты типичных окон — 10–50 мс. Окно рабочего стола шелла
  (`Program Manager`) отвечает провайдеру ~650 мс — это ограничение
  explorer'а, не сервера.
- Для Chrome/Edge/Electron при пустом снапшоте — кликните в окно или
  запустите с `--force-renderer-accessibility`.
- OCR требует установленного языкового пакета Windows (en-US/ru-RU и т.д.).
