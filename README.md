# FastWord

Local, private, push-to-talk dictation for macOS — powered by MLX Whisper running entirely on-device.

Hold a hotkey, speak, release — your words appear in any focused text field. Nothing leaves your Mac.

## Why

Wispr Flow / Superwhisper / Aiko are great, but they ship audio off your machine, charge a subscription, or both. FastWord is:

### RAM footprint vs Wispr Flow

Measured on the same MacBook (M-series, macOS 26), both apps installed and idle in the menu bar:

| State                  | Wispr Flow      | FastWord       |
| ---------------------- | --------------- | -------------- |
| Idle (not dictating)   | **~800 MB**     | **~50 MB**     |
| Active (transcribing)  | ~1 GB           | ~2 GB          |
| Architecture           | Electron + cloud transcription | Native Swift + local MLX Whisper |
| Pricing                | $144/year       | Free, MIT      |

Wispr Flow keeps ~800 MB resident even when you're not dictating because Electron + background JavaScript processes never sleep. FastWord evicts the model from RAM after 10 min of inactivity, so you only pay the memory cost while you're actually speaking. ([Wispr Flow idle RAM is widely reported on Reddit and review sites.](https://www.getvoibe.com/resources/wispr-flow-review/))



- **100% local.** Audio never leaves your Mac. No cloud, no telemetry, no account.
- **Fast.** MLX-accelerated `whisper-large-v3-turbo` on Apple Silicon — typically <1s end-to-end after release.
- **Light when idle.** The model lives in a Python sidecar that **evicts itself from RAM after 10 min of inactivity** (configurable). Your 16GB MacBook keeps its memory.
- **Hackable.** Sidecar architecture means you can swap to faster-whisper, distil-whisper, or Parakeet without touching the Swift app.
- **Tiny menu-bar app.** No Dock icon. Push-to-talk HUD with live equalizer.

## Requirements

- macOS 13 (Ventura) or later
- Apple Silicon (M1/M2/M3/M4) — MLX is Apple Silicon only
- [Homebrew](https://brew.sh)
- Xcode Command Line Tools (`xcode-select --install`)
- Python 3.11+ (`brew install python@3.11` if missing)

## Install

Clone and bootstrap:

```bash
git clone https://github.com/VasenevEA/FastWord.git
cd FastWord
brew install xcodegen

# One-time signing setup — copy the example and fill in your Team ID.
cp LocalConfig.xcconfig.example LocalConfig.xcconfig
# Edit LocalConfig.xcconfig and set DEVELOPMENT_TEAM = YOUR_TEAM_ID
# (find it at developer.apple.com → Membership)

./scripts/bootstrap.sh
./scripts/build.sh
open build/Build/Products/Debug/FastWord.app
```

`bootstrap.sh` creates a Python venv at `~/.fastword/venv`, installs `mlx-whisper`, and copies the sidecar script. `build.sh` generates the Xcode project from `project.yml` and builds the app.

## Permissions

On first launch macOS will ask for:

1. **Microphone** — to record your voice.
2. **Input Monitoring** — so the global hotkey (Right Option) works system-wide.
3. **Accessibility** — so the transcribed text can be auto-pasted into the focused field.

If a prompt doesn't appear, add the app manually in **System Settings → Privacy & Security**:

- Privacy & Security → Input Monitoring → **+** → select `build/Build/Products/Debug/FastWord.app`
- Privacy & Security → Accessibility → **+** → same app

After granting, **fully quit and relaunch FastWord** — TCC permissions only apply on a fresh process start.

## Usage

- **Dictate.** Hold **Right Option (⌥)**, speak, release. Text is pasted at the cursor and saved to history.
- **No focus?** No problem — the transcript is still saved to history.
- **History.** Click the menu-bar icon → **Show History**. Searchable, copyable.
- **Quit.** Menu-bar icon → **Quit**.

## Configuration

Set environment variables before launching the app (or wrap in a launcher script):

| Variable | Default | Notes |
| --- | --- | --- |
| `FASTWORD_MODEL` | `mlx-community/whisper-large-v3-turbo` | Any MLX-Whisper Hugging Face repo |
| `FASTWORD_LANGUAGE` | _(auto-detect)_ | e.g. `en`, `ru`, `de` |
| `FASTWORD_IDLE_EVICT` | `600` | Seconds of inactivity before model is unloaded from RAM |

Models are cached at `~/.cache/huggingface`. The default `turbo` weights are ~1.5 GB.

## Architecture

```
┌──────────────────────┐         stdio JSON         ┌──────────────────────┐
│  FastWord.app        │ ────────────────────────►  │  sidecar.py          │
│  (SwiftUI, AppKit)   │                            │  (mlx-whisper)       │
│                      │ ◄────────────────────────  │                      │
│  - menu bar          │                            │  - lazy-loads model  │
│  - global hotkey     │                            │  - evicts on idle    │
│  - audio capture     │                            │                      │
│  - HUD + history     │                            │                      │
│  - paste injection   │                            │                      │
└──────────────────────┘                            └──────────────────────┘
```

- **Swift app** captures 16 kHz mono Float32 PCM via `AVAudioEngine`, ships it base64-encoded to the sidecar over stdin.
- **Python sidecar** holds the model in RAM between requests, evicts on idle.
- History is a small SQLite DB at `~/.fastword/history.sqlite`.

## Where things live

| Path | What |
| --- | --- |
| `FastWord/Sources/` | Swift sources |
| `sidecar/sidecar.py` | MLX Whisper sidecar (line-delimited JSON over stdio) |
| `scripts/bootstrap.sh` | Sets up `~/.fastword/venv` and installs `mlx-whisper` |
| `scripts/build.sh` | Generates Xcode project, builds app |
| `project.yml` | xcodegen project definition (source of truth) |
| `~/.fastword/venv/` | Python venv used by the sidecar |
| `~/.fastword/sidecar/sidecar.py` | Installed sidecar script |
| `~/.fastword/history.sqlite` | Transcription history |

## Troubleshooting

**Hotkey doesn't trigger anything.** Run `tccutil reset ListenEvent com.fastword.app && tccutil reset Accessibility com.fastword.app`, then re-add the app in System Settings and relaunch.

**Transcription hangs.** Check `~/.fastword/sidecar.log`. If `mlx-whisper` isn't installed in the venv, re-run `./scripts/bootstrap.sh`.

**RAM stays high after dictating.** Wait 10 min — the sidecar evicts the model. Tune with `FASTWORD_IDLE_EVICT`.

**Build fails with `xcodegen: command not found`.** `brew install xcodegen`.

## Languages

The interface is localized into:

- English
- Русский
- 简体中文

Switch in **Settings → Language**, or set automatically based on your macOS preferences.

## Release & distribution

To build a notarized DMG for distribution (e.g. via GitHub Releases or your own site):

1. Have a "Developer ID Application" certificate in your login keychain.
2. Store notarization credentials once:
   ```bash
   xcrun notarytool store-credentials FASTWORD_NOTARY \
       --apple-id "you@example.com" \
       --team-id  "YOUR_TEAM_ID" \
       --password "xxxx-xxxx-xxxx-xxxx"   # app-specific password from appleid.apple.com
   ```
3. Run:
   ```bash
   ./scripts/release.sh
   ```
   The signed, notarized DMG lands at `build/release/FastWord-<version>.dmg`.

To publish on GitHub:
```bash
gh release create v0.1.0 build/release/FastWord-0.1.0.dmg \
    --title "FastWord 0.1.0" --generate-notes
```

## License

MIT.
