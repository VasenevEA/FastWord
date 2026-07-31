# Downloads a ggml whisper.cpp model into the location fastword-win expects:
# %LOCALAPPDATA%\fastword\models\ggml-<model>.bin
#
# Usage:
#   .\get-model.ps1                       # large-v3-turbo-q5_0 (~547 MB, default)
#   .\get-model.ps1 -Model small          # any model name from ggerganov/whisper.cpp
#
# If you pick a non-default model, point model_path in
# %APPDATA%\FastWord\config.toml at the downloaded file.

param(
    [string]$Model = "large-v3-turbo-q5_0"
)

$ErrorActionPreference = "Stop"

$fileName = "ggml-$Model.bin"
$url = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$fileName"
$dir = Join-Path $env:LOCALAPPDATA "fastword\models"
$dest = Join-Path $dir $fileName

if (Test-Path $dest) {
    Write-Host "Already downloaded: $dest"
    exit 0
}

New-Item -ItemType Directory -Force -Path $dir | Out-Null

Write-Host "Downloading $url"
Write-Host "        -> $dest"
$tmp = "$dest.partial"
try {
    # BITS gives a progress bar and resumable download; fall back to
    # Invoke-WebRequest if the BITS service is unavailable.
    Start-BitsTransfer -Source $url -Destination $tmp -DisplayName "FastWord model $Model"
} catch {
    Write-Host "BITS unavailable ($($_.Exception.Message)); using Invoke-WebRequest"
    Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
}
Move-Item -Force $tmp $dest
Write-Host "Done: $dest"
