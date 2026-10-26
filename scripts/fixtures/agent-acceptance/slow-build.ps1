$ErrorActionPreference='Stop'
Set-Content -LiteralPath build.pid -Value $PID
1..6 | ForEach-Object { Write-Output "build-progress-$_"; Start-Sleep -Seconds 1 }
& rustc --edition 2021 build-input.rs -o build-output.exe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& ./build-output.exe | Set-Content -LiteralPath build-done.txt
