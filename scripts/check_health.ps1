$outFile = "frontend_stdout.txt"
$errFile = "frontend_stderr.txt"
$process = Start-Process -FilePath "cargo" -ArgumentList "run", "--bin", "service_frontend" -PassThru -NoNewWindow -RedirectStandardOutput $outFile -RedirectStandardError $errFile
Write-Output "Started process $($process.Id). Waiting for port 3000..."

function Invoke-EndpointSafe {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Uri
    )

    $tmp = [System.IO.Path]::GetTempFileName()
    try {
        $statusRaw = & curl.exe -sS -o $tmp -w "%{http_code}" --max-time 10 $Uri 2>$null
        $body = ""
        if (Test-Path $tmp) {
            $body = Get-Content -Path $tmp -Raw -ErrorAction SilentlyContinue
        }

        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($statusRaw)) {
            return @{ Ok = $false; StatusCode = $null; Content = $body; Error = "curl request failed" }
        }

        $statusCode = [int]$statusRaw
        if ($statusCode -ge 200 -and $statusCode -lt 400) {
            return @{ Ok = $true; StatusCode = $statusCode; Content = $body; Error = $null }
        }

        return @{ Ok = $false; StatusCode = $statusCode; Content = $body; Error = "non-success status returned" }
    } catch {
        $errorMessage = if ($_.Exception) { $_.Exception.Message } else { $_.ToString() }
        return @{ Ok = $false; StatusCode = $null; Content = $null; Error = $errorMessage }
    } finally {
        if (Test-Path $tmp) {
            Remove-Item $tmp -ErrorAction SilentlyContinue
        }
    }
}

$portOpen = $false
for ($i = 0; $i -lt 60; $i++) {
    try {
        $conn = Test-NetConnection -ComputerName localhost -Port 3000 -InformationLevel Quiet
        if ($conn) {
            $portOpen = $true
            break
        }
    } catch {}
    Start-Sleep -Seconds 5
}

if (-not $portOpen) {
    Write-Output "Timeout waiting for port 3000."
} else {
    Write-Output "Port 3000 is open. Testing endpoints..."

    $health = Invoke-EndpointSafe -Uri "http://localhost:3000/health"
    if ($health.Ok) {
        Write-Output "HEALTH: $($health.StatusCode) $($health.Content)"
    } else {
        Write-Output "HEALTH ERROR: $($health.Error)"
        if ($null -ne $health.StatusCode) { Write-Output "Status: $($health.StatusCode)" }
    }

    $status = Invoke-EndpointSafe -Uri "http://localhost:3000/api/status"
    if ($status.Ok) {
        Write-Output "STATUS: $($status.StatusCode) $($status.Content)"
    } else {
        Write-Output "STATUS ERROR: $($status.Error)"
        if ($null -ne $status.StatusCode) { Write-Output "Status: $($status.StatusCode)" }
    }
}

if ($process -and -not $process.HasExited) {
    Stop-Process -Id $process.Id -Force
}
Write-Output "--- STDOUT ---"
if (Test-Path $outFile) { Get-Content $outFile | Select-Object -Last 20 }
Write-Output "--- STDERR ---"
if (Test-Path $errFile) { Get-Content $errFile | Select-Object -Last 20 }
