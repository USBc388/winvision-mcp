$exe = "F:\skill creation\winvision-mcp\target\release\winvision-mcp.exe"

function Test-Spawn($noWindow) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $noWindow
    $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $p = [System.Diagnostics.Process]::Start($psi)
    $stdin = $p.StandardInput
    $stdout = $p.StandardOutput
    $stdin.WriteLine('{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'); $stdin.Flush()
    $null = $stdout.ReadLine()
    $stdin.WriteLine('{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ocr_read_region","arguments":{"x":0,"y":0,"w":400,"h":100}}}'); $stdin.Flush()
    $resp = $stdout.ReadLine() | ConvertFrom-Json
    $label = if ($noWindow) { "CreateNoWindow=true  " } else { "CreateNoWindow=false " }
    $t = $resp.result.content[0].text
    if ($resp.result.isError) { Write-Host "$label -> ERROR: $($t.Substring(0, [Math]::Min(120, $t.Length)))" }
    else { Write-Host "$label -> OK: $($t -split "`n" | Select-Object -First 1)" }
    if (-not $p.HasExited) { $p.Kill() }
}

Test-Spawn $true
Test-Spawn $false
Test-Spawn $true
Test-Spawn $false
