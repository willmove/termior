Set-Content -LiteralPath background.pid -Value $PID
Set-Content -LiteralPath bg-started.txt -Value started
while ($true) { Write-Output 'service-tick'; Start-Sleep -Seconds 1 }
