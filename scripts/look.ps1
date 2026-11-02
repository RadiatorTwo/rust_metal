# Measurement run: deletes the savegames, builds, starts the game with recording,
# sampler and frame log. Run: powershell -ExecutionPolicy Bypass -File scripts\look.ps1 [name]
# Writes <name>.txt (the recording), <name>.log, <name>_frames.txt; name defaults to "look".
param([string]$name = "look")
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
Remove-Item "$root\game_files\data\SavedGames\*" -ErrorAction SilentlyContinue
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$env:GUNMETAL_RECORD = "$name.txt"
$env:GUNMETAL_SAMPLE = "1"
$env:GUNMETAL_FRAMES = "${name}_frames.txt"
cmd /c "target\release\gunmetal.exe 2> $name.log"
Remove-Item Env:GUNMETAL_RECORD, Env:GUNMETAL_SAMPLE, Env:GUNMETAL_FRAMES
Write-Host "Done: $name.txt, $name.log, ${name}_frames.txt"
