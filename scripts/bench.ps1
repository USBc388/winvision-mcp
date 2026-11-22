
$ErrorActionPreference = "Stop"
$exe = "F:\skill creation\winvision-mcp\target\release\winvision-mcp.exe"

function Req($id, $tool, $argsJson) {
    '{"jsonrpc":"2.0","id":' + $id + ',"method":"tools/call","params":{"name":"' + $tool + '","arguments":' + $argsJson + '}}'
}

function Stats($samples) {
    $s = $samples | Sort-Object
    $n = $s.Count
    $min = $s[0]
    $max = $s[$n - 1]
    $avg = [math]::Round(($s | Measure-Object -Average).Average, 1)
    $p50 = $s[[int][math]::Floor(($n - 1) * 0.5)]
    $p95 = $s[[int][math]::Floor(($n - 1) * 0.95)]
    return "min $min / p50 $p50 / p95 $p95 / avg $avg / max $max ms"
}

function Get-FirstText($respJson) {
    $j = $respJson | ConvertFrom-Json
    if ($j.result.content) { return $j.result.content[0].text } else { return "" }
}

function Measure-Tool($script:stdin, $script:stdout, $idBase, $tool, $argsJson, $warmups, $iters) {
    $id = $idBase * 1000
    for ($i = 0; $i -lt $warmups; $i++) {
        $script:stdin.WriteLine((Req (++$id) $tool $argsJson)); $script:stdin.Flush()
        $null = $script:stdout.ReadLine()
    }
    $samples = New-Object System.Collections.Generic.List[double]
    for ($i = 0; $i -lt $iters; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $script:stdin.WriteLine((Req (++$id) $tool $argsJson)); $script:stdin.Flush()
        $resp = $script:stdout.ReadLine()
        $sw.Stop()
        $j = $resp | ConvertFrom-Json
        if ($j.result.isError) { throw "tool $tool returned error: $($j.result.content[0].text)" }
        $samples.Add($sw.Elapsed.TotalMilliseconds)
        Start-Sleep -Milliseconds 20
    }
    return $samples
}

function Machine-Specs {
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name.Trim()
    $gpu = (Get-CimInstance Win32_VideoController | Select-Object -First 1).Name
    $os  = (Get-CimInstance Win32_OperatingSystem).Caption
    $ver = (Get-CimInstance Win32_OperatingSystem).Version
    $ram = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 0)
    Add-Type -AssemblyName System.Windows.Forms
    $res = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    return "CPU: $cpu  |  GPU: $gpu  |  RAM: $ram GB  |  $os ($ver)  |  display $($res.Width)x$($res.Height)"
}


Write-Host "Machine: $(Machine-Specs)"

Start-Process taskkill -ArgumentList "/IM notepad.exe /F" -WindowStyle Hidden -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exe
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
$psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
$p = [System.Diagnostics.Process]::Start($psi)
$stdin = $p.StandardInput
$stdout = $p.StandardOutput

$stdin.WriteLine('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bench","version":"0"}}}')
$stdin.Flush()
$null = $stdout.ReadLine()
$stdin.WriteLine('{"jsonrpc":"2.0","method":"notifications/initialized"}'); $stdin.Flush()

try {

$stdin.WriteLine((Req 100 "window_manager" '{"action":"launch_app","command":"notepad"}')); $stdin.Flush()
$null = $stdout.ReadLine()
$stdin.WriteLine((Req 101 "window_manager" '{"action":"wait_for_window","target":"notepad","timeout_ms":10000,"poll_ms":150}')); $stdin.Flush()
$waitResp = $stdout.ReadLine()
$waitText = Get-FirstText $waitResp
Write-Host "target: $waitText"

# COLD snapshot (first UIA call in the process = COM init + first provider contact)
$swCold = [System.Diagnostics.Stopwatch]::StartNew()
$stdin.WriteLine((Req 102 "desktop_snapshot" '{"target_window":"notepad"}')); $stdin.Flush()
$snap1 = $stdout.ReadLine()
$swCold.Stop()

$editId = $null
for ($try = 0; $try -lt 6; $try++) {
    
    $m = [regex]::Match((Get-FirstText $snap1), '\[(\d+)\] (Edit|Document)')
    if ($m.Success) { $editId = $m.Groups[1].Value; break }
    Start-Sleep -Milliseconds 300
    $stdin.WriteLine((Req (103 + $try) "desktop_snapshot" '{"target_window":"notepad"}')); $stdin.Flush()
    $snap1 = $stdout.ReadLine()
}
Write-Host "cold snapshot: $([math]::Round($swCold.Elapsed.TotalMilliseconds,1)) ms; edit element_id=$editId"
if (-not $editId) {
    Write-Host "SNAPSHOT DUMP (no Edit found):"
    Write-Host (Get-FirstText $snap1)
}


$snapSamples = Measure-Tool $stdin $stdout 2 "desktop_snapshot" '{"target_window":"notepad"}' 1 15
$focusSamples = $null
$setValueSamples = $null
if ($editId) {
    $focusSamples = Measure-Tool $stdin $stdout 3 "ui_interact" ('{"element_id":' + $editId + ',"action":"focus"}') 1 10
    $setValueSamples = Measure-Tool $stdin $stdout 4 "ui_interact" ('{"element_id":' + $editId + ',"action":"set_value","text":"winvision benchmark 123"}') 1 5
}
$ocrSamples = Measure-Tool $stdin $stdout 5 "ocr_read_region" '{"target_window":"notepad"}' 1 10
$somSamples = Measure-Tool $stdin $stdout 6 "visual_som_capture" '{"target_window":"notepad","annot":"windows","max_width":1280}' 1 10
$winsSamples = Measure-Tool $stdin $stdout 7 "window_manager" '{"action":"list_windows"}' 1 10
$clipSamples = Measure-Tool $stdin $stdout 8 "clipboard" '{"action":"get"}' 1 10

$stdin.WriteLine((Req 900 "window_manager" '{"action":"list_windows"}')); $stdin.Flush()
$wins = Get-FirstText ($stdout.ReadLine())
if ($wins -match 'pid (\d+)\] "[^"]*" \S*Notepad') {
    $npid = $Matches[1]
    Start-Process taskkill -ArgumentList "/PID $npid /F" -WindowStyle Hidden
}
} finally {
    if ($p -and -not $p.HasExited) { $p.Kill() }
}

$spec = Machine-Specs
$rustv = (rustc --version)
$md = @"
# WinVision benchmark results

Measured on: $spec
Binary: release build (LTO, codegen-units=1, strip), $rustv
Protocol: MCP over stdio, per-call latency = full JSON-RPC round trip (write + response), measured client-side in PowerShell.

| Tool | Samples | Latency (min / p50 / p95 / avg / max) |
|---|---|---|
| desktop_snapshot (cold, first call in process) | 1 | $([math]::Round($swCold.Elapsed.TotalMilliseconds,1)) ms |
| desktop_snapshot (warm, Notepad window) | $($snapSamples.Count) | $(Stats $snapSamples) |
| ui_interact focus (UIA SetFocus) | $(if ($focusSamples) { $focusSamples.Count } else { 0 }) | $(if ($focusSamples) { Stats $focusSamples } else { "n/a" }) |
| ui_interact set_value (UIA ValuePattern) | $(if ($setValueSamples) { $setValueSamples.Count } else { 0 }) | $(if ($setValueSamples) { Stats $setValueSamples } else { "n/a" }) |
| ocr_read_region (DXGI grab + Windows.Media.Ocr) | $($ocrSamples.Count) | $(Stats $ocrSamples) |
| visual_som_capture (grab + JPEG encode + badges) | $($somSamples.Count) | $(Stats $somSamples) |
| window_manager list_windows (EnumWindows full scan) | $($winsSamples.Count) | $(Stats $winsSamples) |
| clipboard get | $($clipSamples.Count) | $(Stats $clipSamples) |
"@

Set-Content -Path "F:\skill creation\winvision-mcp\bench-results.md" -Value $md -Encoding UTF8
Write-Host $md
