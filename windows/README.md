# FastWord for Windows

Local, private, push-to-talk dictation for Windows — the same `whisper.cpp`
sidecar as the macOS app, with a native Rust tray client instead of the
Swift one.

Hold a hotkey (default: **Right Ctrl**), speak, release — your words are
pasted into the focused text field. Nothing leaves your PC.

> **Status: experimental.** CPU inference only for now (no GPU backend),
> GigaAM is macOS-only, and there is no settings/history UI — configuration
> is a TOML file and history is a SQLite file.

## Build from source

Prerequisites:

- [Rust](https://rustup.rs/) with the MSVC toolchain
- **Visual Studio Build Tools** with the *Desktop development with C++*
  workload (provides `cl.exe` and CMake)
- `libclang.dll` for bindgen — install [LLVM](https://llvm.org/)
  (`winget install LLVM.LLVM`) or set `LIBCLANG_PATH` to any folder
  containing `libclang.dll` (the `libclang` Python wheel is a lightweight
  source: `pip download libclang`, extract the `.whl` as a zip)

Then:

```powershell
# From the repo root — builds sidecar + app, stages exes together.
powershell -ExecutionPolicy Bypass -File windows\scripts\build.ps1

# Download the Whisper model (~547 MB, one-time).
powershell -ExecutionPolicy Bypass -File windows\scripts\get-model.ps1

# Run.
windows\target\release\fastword-win.exe
```

A tray icon appears. Hold **Right Ctrl**, dictate, release. Press
**Escape** during recording (or while transcription is in flight) to cancel.

The first dictation after launch is preceded by a model warm-up
(~seconds); after `idle_evict_secs` without use the model is dropped from
RAM and the next dictation warms up again.

## Configuration

`%APPDATA%\FastWord\config.toml` — created with defaults on first launch
(tray icon → *Open config*). Restart the app after editing.

| Key | Default | Notes |
| --- | --- | --- |
| `hotkey` | `right_ctrl` | `left_ctrl`, `right_alt`, `f13`…`f24`, `caps_lock`, … |
| `hotkey_secondary` | *(empty)* | optional second key |
| `combo_mode` | `either` | `either` — any key alone; `both` — chord |
| `language` | `system` | `system`, `auto`, or ISO code (`ru`, `en`, …) |
| `model_path` | *(empty)* | empty → `%LOCALAPPDATA%\fastword\models\ggml-large-v3-turbo-q5_0.bin` |
| `idle_evict_secs` | `600` | seconds before the model is evicted from RAM |
| `skip_empty` | `true` | drop clearly-silent recordings |
| `cleanup_enabled` | `true` | strip Whisper artefacts (credits, repeats) |
| `beeps` | `true` | audible record start/error cues |

Slow machine? Download a smaller model and point `model_path` at it:

```powershell
powershell -ExecutionPolicy Bypass -File windows\scripts\get-model.ps1 -Model small
```

## Where things live

| Path | What |
| --- | --- |
| `windows/src/` | Tray app sources (Rust) |
| `%APPDATA%\FastWord\config.toml` | Settings |
| `%LOCALAPPDATA%\fastword\models\` | Whisper models |
| `%LOCALAPPDATA%\fastword\fastword-win.log` | Log (tray → *Open log*) |
| `~\.fastword\history.sqlite` | Transcription history (same schema as macOS) |

## Architecture

Same two-process design as macOS — only the UI layer differs:

```
┌──────────────────────┐         stdio JSON         ┌────────────────────────┐
│  fastword-win.exe    │ ────────────────────────►  │  fastword-sidecar.exe  │
│  (Rust, Win32)       │                            │  (Rust + whisper.cpp)  │
│                      │ ◄────────────────────────  │                        │
│  - tray icon         │                            │  - lazy-loads model    │
│  - WH_KEYBOARD_LL    │                            │  - CPU inference       │
│    push-to-talk      │                            │  - evicts on idle      │
│  - WASAPI capture    │                            │                        │
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

## Tests

```powershell
cd windows ; cargo test        # app: DSP, protocol, config, hotkey, post-processor, history
cd sidecar-rust ; cargo test   # sidecar: protocol + PCM decoding
```

## Not ported yet

- GPU acceleration (whisper.cpp Vulkan/CUDA backends are a build flag away)
- GigaAM-v3 Russian engine (sherpa-onnx is cfg-gated to macOS)
- Settings / history / model-management UI, HUD, live preview
- Localized UI strings
