# Builds the Rust sidecar and the Windows tray app, and stages both exes in
# windows\target\release\ (the app looks for fastword-sidecar.exe next to
# itself first).
#
# Prerequisites:
#   * Rust (rustup.rs), MSVC toolchain
#   * Visual Studio Build Tools with the C++ workload (cl.exe + CMake)
#   * libclang for bindgen — see the LIBCLANG_PATH note below
#
# Usage:  powershell -ExecutionPolicy Bypass -File scripts\build.ps1

$ErrorActionPreference = "Stop"
$repo = Resolve-Path (Join-Path $PSScriptRoot "..\..")

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

# --- libclang for bindgen (whisper-rs-sys) ---------------------------------
if (-not $env:LIBCLANG_PATH) {
    $candidates = @(
        "$env:ProgramFiles\LLVM\bin"
    )
    foreach ($c in $candidates) {
        if (Test-Path (Join-Path $c "libclang.dll")) { $env:LIBCLANG_PATH = $c; break }
    }
    if (-not $env:LIBCLANG_PATH) {
        throw @"
libclang.dll not found — bindgen needs it to generate whisper.cpp bindings.
Either install LLVM (winget install LLVM.LLVM), or extract libclang.dll from
the 'libclang' Python wheel (pip download libclang) into a folder and set
LIBCLANG_PATH to that folder.
"@
    }
}
Write-Host "LIBCLANG_PATH = $env:LIBCLANG_PATH"

# --- build -----------------------------------------------------------------
# The cmake crate strips /O flags from its computed MSVC flags AND overrides
# CMAKE_C(XX)_FLAGS_RELEASE, clobbering CMake's default "/O2 /Ob2 /DNDEBUG" —
# whisper.cpp then builds with no optimization at all and transcribes ~15x
# slower. whisper-rs-sys passes CMAKE_* env vars through as -D defines, and
# the cmake crate skips its override for variables that are already defined,
# so setting them here restores an optimized build. (SIMD/AVX2 is detected
# and applied per-target by ggml's own cmake via GGML_NATIVE.)
$env:CMAKE_C_FLAGS_RELEASE = "/O2 /Ob2 /DNDEBUG"
$env:CMAKE_CXX_FLAGS_RELEASE = "/O2 /Ob2 /DNDEBUG"
# sherpa-onnx (GigaAM) ships a static-MT prebuilt; build the rust CRT and
# whisper.cpp with the static runtime so the linker agrees.
# remap-path-prefix keeps local usernames/dir layout out of the panic
# location strings embedded in release binaries.
$env:RUSTFLAGS = "-C target-feature=+crt-static --remap-path-prefix=$env:USERPROFILE=~ --remap-path-prefix=$repo=."

Write-Host "`n=== Building sidecar (whisper.cpp — first build takes a while) ==="
Push-Location (Join-Path $repo "sidecar-rust")
try { cargo build --release } finally { Pop-Location }

Write-Host "`n=== Building fastword-win ==="
Push-Location (Join-Path $repo "windows")
try { cargo build --release } finally { Pop-Location }

# --- stage -----------------------------------------------------------------
$stage = Join-Path $repo "windows\target\release"
Copy-Item (Join-Path $repo "sidecar-rust\target\release\fastword-sidecar.exe") $stage -Force

Write-Host "`nDone:"
Write-Host "  $stage\fastword-win.exe"
Write-Host "  $stage\fastword-sidecar.exe"
Write-Host "`nModel: run scripts\get-model.ps1 if you haven't yet, then start fastword-win.exe."
