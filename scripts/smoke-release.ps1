# WinVision release-binary smoke test via PowerShell (JSON-RPC over stdio).
$exe = "F:\skill creation\winvision-mcp\target\release\winvision-mcp.exe"

$init  = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"ps-smoke","version":"0"}}}'
$notif = '{"jsonrpc":"2.0","method":"notifications/initialized"}'
$list  = '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
$snap  = '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"desktop_snapshot","arguments":{"target_window":"Program Manager"}}}'
$wins  = '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"window_manager","arguments":{"action":"list_windows"}}}'

$input = "$init`n$notif`n$list`n$snap`n$wins`n"

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$out = $input | & $exe 2>$null
$sw.Stop()

Write-Host "=== total round-trip: $($sw.ElapsedMilliseconds) ms ==="
foreach ($line in $out) {
    $j = $line | ConvertFrom-Json
    switch ($j.id) {
        1 { Write-Host "[initialize] protocol=$($j.result.protocolVersion) server=$($j.result.serverInfo.name) v$($j.result.serverInfo.version)" }
        2 { $names = ($j.result.tools | ForEach-Object { $_.name }) -Join ', '; Write-Host "[tools/list] tools=$($j.result.tools.Count): $names" }
        3 {
            $t = $j.result.content[0].text
            $lines = $t -split "`n"
            Write-Host "[desktop_snapshot] isError=$($j.result.isError)"
            Write-Host "  $($lines[0])"
            Write-Host "  $($lines[3])"
        }
        4 {
            $t = $j.result.content[0].text
            $first = ($t -split "`n")[0]
            Write-Host "[list_windows] isError=$($j.result.isError) : $first"
        }
    }
}
