# FastWord for Windows

Local, private, push-to-talk dictation for Windows — the same `whisper.cpp`
sidecar as the macOS app, with a native Rust tray client instead of the
Swift one.

Hold a hotkey (default: **Right Ctrl**), speak, release — your words are
pasted into the focused text field. Nothing leaves your PC.

## Install

Grab `FastWord-Setup-<version>.exe` from CI artifacts (or build it
yourself, see below) and run it — a per-user install, no admin rights
needed. On first launch FastWord offers to download a speech model
(~550 MB) into `%LOCALAPPDATA%\fastword\models\`.

A tray icon appears. Hold **Right Ctrl**, dictate, release; an on-screen
HUD (like the macOS one) shows recording and transcription state. Press
**Escape** to cancel. Right-click the tray icon for **History**,
**Settings** and **Models**.

## Features

- **Push-to-talk dictation** into any focused text field (clipboard +
  Ctrl+V injection with clipboard restore)
- **On-screen HUD** — 5-bar equalizer driven by the live mic level
- **History window** — search, copy, delete (stored in
  `~\.fastword\history.sqlite`, same schema as macOS)
- **Settings window** — hotkey (plus optional second key), language,
  model idle-unload, cleanup toggles, sound cues, start-with-Windows
- **Models window** — the same catalog as macOS: nine multilingual
  Whisper models plus **GigaAM v3**, Sber's Russian-specific model
  (~50% lower WER than Whisper on Russian); download, switch, delete
- **GPU transcription** — with an NVIDIA GPU and the CUDA toolkit the
  sidecar runs ~20x faster than CPU; FastWord detects the GPU and offers
  the setup once
- **Localized UI** — English, Русский, 简体中文 (follows the system
  language)

## Build from source

Prerequisites:

- [Rust](https://rustup.rs/) with the MSVC toolchain
- **Visual Studio Build Tools** with the *Desktop development with C++*
  workload (provides `cl.exe`, CMake and Ninja)
- `libclang.dll` for bindgen — install [LLVM](https://llvm.org/)
  (`winget install LLVM.LLVM`) or set `LIBCLANG_PATH` to any folder
  containing `libclang.dll`

Then:

```powershell
# From the repo root — builds sidecar + app, stages exes together.
powershell -ExecutionPolicy Bypass -File windows\scripts\build.ps1

# Optional: build the installer (needs Inno Setup 6).
iscc windows\installer\fastword.iss

# Run directly without installing:
windows\target\release\fastword-win.exe
```

CI (`.github/workflows/windows.yml`) runs tests and produces the
installer for every push touching `windows/` or `sidecar-rust/`.

### GPU (CUDA)

With an NVIDIA GPU, transcription runs roughly 10–20x faster than on CPU.
FastWord detects this itself: if it finds an NVIDIA driver while running a
CPU-only engine, it offers the setup once at startup. To enable manually:

```powershell
powershell -ExecutionPolicy Bypass -File windows\scripts\setup-gpu.ps1
```

The script installs a CUDA toolkit compatible with the installed driver
(one UAC confirmation, several GB) and rebuilds the sidecar with
`--features cuda`. Kernels are built for RTX 30xx by default — pass
`-Arch 89` for RTX 40xx, `-Arch 75` for RTX 20xx/GTX 16xx. After a
restart the log shows `backend=cuda`.

Note: the toolkit version must not exceed what the GPU driver supports
(`nvidia-smi` prints the driver's ceiling as "CUDA Version") — a newer
runtime builds fine but silently falls back to CPU. The script picks a
compatible version automatically.

### Performance

Measured on a 5.4 s clip (18-core Haswell Xeon, RTX 3060), default
`large-v3-turbo-q5_0`:

| Engine | Transcription time |
| --- | --- |
| Whisper, GPU (CUDA) | **0.5 s** |
| GigaAM v3, CPU (Russian) | 1.4 s |
| Whisper, CPU (AVX2, 18 threads) | 11 s |

On a slow machine pick a smaller Whisper model in the Models window —
`small` is roughly 10x faster than `large-v3-turbo` at noticeably lower
accuracy.

## Where things live

| Path | What |
| --- | --- |
| `windows/src/` | Tray app sources (Rust) |
| `windows/installer/` | Inno Setup script |
| `%APPDATA%\FastWord\config.toml` | Settings (editable in the UI) |
| `%LOCALAPPDATA%\fastword\models\` | Whisper models + `gigaam-v3-ctc\` |
| `%LOCALAPPDATA%\fastword\fastword-win.log` | Log (tray → *Open log*) |
| `~\.fastword\history.sqlite` | Transcription history (same schema as macOS) |

## Architecture

Same two-process design as macOS — only the UI layer differs:

```
┌──────────────────────┐         stdio JSON         ┌────────────────────────┐
│  fastword-win.exe    │ ────────────────────────►  │  fastword-sidecar.exe  │
│  (Rust, Win32)       │                            │  (Rust + whisper.cpp   │
│                      │ ◄────────────────────────  │        + sherpa-onnx)  │
│  - tray + tabbed UI  │                            │  - lazy-loads model    │
│  - WH_KEYBOARD_LL    │                            │  - CPU or CUDA         │
│    push-to-talk      │                            │  - GigaAM (Russian)    │
│  - WASAPI capture    │                            │  - evicts on idle      │
│  - HUD overlay       │                            │                        │
│  - Ctrl+V injection  │                            │                        │
└──────────────────────┘                            └────────────────────────┘
```

- Audio is captured from the default input device via WASAPI (cpal),
  downmixed to mono and resampled to 16 kHz Float32 — the sidecar protocol
  is byte-for-byte the one the Swift app speaks.
- Insertion writes the transcript to the clipboard, synthesizes Ctrl+V and
  restores your previous clipboard text ~700 ms later (unless you copied
  something else meanwhile). Non-text clipboard content (images, files) is
  not restored — known limitation.
- Transcripts run through the same post-processing pipeline as macOS
  (known-hallucination phrases, repetition loops, bracketed noise).
- Everything links against the static CRT (the sherpa-onnx prebuilt is
  static-MT), so no VC redistributable is required at runtime.

## Tests

```powershell
cd windows ; cargo test        # app: DSP, protocol, config, hotkey, post-processor, history
cd sidecar-rust ; cargo test   # sidecar: protocol + PCM decoding
```

## Not ported yet

- Live preview of the transcript while recording
- Vulkan GPU backend for non-NVIDIA GPUs (the `vulkan` cargo feature
  exists on the sidecar but needs the Vulkan SDK at build time)
