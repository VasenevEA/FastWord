# Enables GPU (CUDA) transcription for FastWord on Windows:
#   1. installs the NVIDIA CUDA toolkit if nvcc is missing (winget; asks for
#      one admin/UAC confirmation),
#   2. rebuilds fastword-sidecar with whisper.cpp's CUDA backend,
#   3. stages the new exe next to fastword-win.exe.
#
# Requires an NVIDIA GPU with a driver installed. Expect the CUDA install
# to download several GB and the rebuild to take ~10 minutes.
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\setup-gpu.ps1
#   -Arch <sm>   CUDA architecture to compile kernels for. Default 86
#                (RTX 30xx / Ampere). 89 = RTX 40xx, 75 = RTX 20xx/GTX 16xx,
#                120 = RTX 50xx. Building fewer architectures is much faster.

param(
    [string]$Arch = "86"
)

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..")

# --- sanity: NVIDIA driver -------------------------------------------------
if (-not (Test-Path (Join-Path $env:WINDIR "System32\nvml.dll"))) {
    throw "No NVIDIA driver detected (System32\nvml.dll missing). Install the GPU driver first."
}

# --- CUDA toolkit ----------------------------------------------------------
function Find-Nvcc {
    $cmd = Get-Command nvcc.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $root = "$env:ProgramFiles\NVIDIA GPU Computing Toolkit\CUDA"
    if (Test-Path $root) {
        $ver = Get-ChildItem $root -Directory | Sort-Object Name -Descending | Select-Object -First 1
        $nvcc = Join-Path $ver.FullName "bin\nvcc.exe"
        if (Test-Path $nvcc) { return $nvcc }
    }
    return $null
}

$nvcc = Find-Nvcc
if (-not $nvcc) {
    Write-Host "CUDA toolkit not found - installing via winget (confirm the UAC prompt)..."
    winget install --id Nvidia.CUDA -e --accept-source-agreements --accept-package-agreements
    if ($LASTEXITCODE -ne 0) { throw "winget install Nvidia.CUDA failed (exit $LASTEXITCODE)" }
    $nvcc = Find-Nvcc
    if (-not $nvcc) { throw "nvcc still not found after install - open a new terminal and re-run this script." }
}
$cudaBin = Split-Path $nvcc
$cudaHome = Split-Path $cudaBin
Write-Host "CUDA: $cudaHome"
$env:CUDA_PATH = $cudaHome
$env:PATH = "$cudaBin;$env:PATH"

# --- MSVC environment (cl.exe, cmake) --------------------------------------
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        throw "Visual Studio Build Tools not found. Install them (with the C++ workload) first."
    }
    $vsPath = & $vswhere -products * -latest -property installationPath
    $vcvars = Join-Path $vsPath "VC\Auxiliary\Build\vcvars64.bat"
    Write-Host "Importing MSVC environment from $vcvars"
    $envDump = cmd /c "`"$vcvars`" >nul 2>&1 && set"
    foreach ($line in $envDump) {
        if ($line -match "^([^=]+)=(.*)$") {
            [System.Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], "Process")
        }
    }
}

# --- libclang for bindgen --------------------------------------------------
if (-not $env:LIBCLANG_PATH) {
    $candidates = @(
        "$env:ProgramFiles\LLVM\bin"
    )
    foreach ($c in $candidates) {
        if (Test-Path (Join-Path $c "libclang.dll")) { $env:LIBCLANG_PATH = $c; break }
    }
    if (-not $env:LIBCLANG_PATH) { throw "libclang.dll not found - see build.ps1 for options." }
}

# --- build -----------------------------------------------------------------
# See build.ps1 for why the CMAKE_*_FLAGS_RELEASE overrides are needed
# (the cmake crate otherwise produces an unoptimized MSVC build).
$env:CMAKE_C_FLAGS_RELEASE = "/O2 /Ob2 /DNDEBUG"
$env:CMAKE_CXX_FLAGS_RELEASE = "/O2 /Ob2 /DNDEBUG"
# Compile kernels only for the requested GPU generation - much faster build.
$env:CMAKE_CUDA_ARCHITECTURES = $Arch

Write-Host "`n=== Building sidecar with CUDA (takes a while - nvcc compiles GPU kernels) ==="
Push-Location (Join-Path $repo "sidecar-rust")
try { cargo build --release --features cuda } finally { Pop-Location }

$stage = Join-Path $repo "windows\target\release"
if (Test-Path $stage) {
    Copy-Item (Join-Path $repo "sidecar-rust\target\release\fastword-sidecar.exe") $stage -Force
    Write-Host "Staged: $stage\fastword-sidecar.exe"
}

Write-Host "`nDone. Restart fastword-win.exe - the log should now show 'backend=cuda'."
