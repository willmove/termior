Set-Content -LiteralPath cancel.pid -Value $PID
Set-Content -LiteralPath cancel-started.txt -Value started
Start-Sleep -Seconds 60
Set-Content -LiteralPath cancel-forbidden.txt -Value forbidden
