# MediaPlayer

A lightweight, modern video player built in Rust — inspired by the Windows 11 Media Player aesthetic.
Dark theme, full-bleed video, amber seek bar, clean minimal controls. No ads. No telemetry. No bloat.

## Supported Formats
MP4, MKV, AVI, MOV, WMV, FLV, WebM, M4V, TS, MPG, MPEG, 3GP, OGV — anything FFmpeg can decode.

## Controls

| Action | Input |
|---|---|
| Play / Pause | `Space` or click the ⏸/▶ button |
| Seek | Click or drag the seek bar |
| Skip ±10s | `← →` arrow keys or ⏮⏭ buttons |
| Volume | `↑ ↓` arrow keys or drag the volume bar |
| Fullscreen | `F` key or ⛶ button |
| Exit fullscreen | `Escape` |
| Next file | `N` key |
| Previous file | `P` key |
| Open file | Drag & drop onto the window |

---

## Building on Windows

### Step 1 — Install Rust
Download and run the installer from https://rustup.rs
Accept all defaults. This installs `rustc` and `cargo`.

### Step 2 — Install FFmpeg

FFmpeg must be available for the `ffmpeg-next` crate to link against.
The easiest way on Windows is via `vcpkg`:

```powershell
# Install vcpkg (if you don't have it)
git clone https://github.com/microsoft/vcpkg.git C:\vcpkg
cd C:\vcpkg
.\bootstrap-vcpkg.bat

# Install FFmpeg (this takes a few minutes)
.\vcpkg install ffmpeg:x64-windows-static

# Set environment variables so Rust can find it
$env:VCPKG_ROOT = "C:\vcpkg"
$env:FFMPEG_DIR = "C:\vcpkg\installed\x64-windows-static"
```

**Alternative: pre-built FFmpeg**
Download the "shared" build from https://www.gyan.dev/ffmpeg/builds/
Extract it and set:
```powershell
$env:FFMPEG_DIR = "C:\path\to\ffmpeg"
```

### Step 3 — Install Visual Studio Build Tools
Required for linking on Windows.
Download from: https://visualstudio.microsoft.com/visual-cpp-build-tools/
Install the **"Desktop development with C++"** workload.

### Step 4 — Build

```powershell
cd path\to\mediaplayer
cargo build --release
```

Your `.exe` will be at:
```
target\release\mediaplayer.exe
```

---

## Optional: Associate with video files

To make double-clicking `.mp4` / `.mkv` / etc. open MediaPlayer:
1. Right-click any video file → "Open with" → "Choose another app"
2. Browse to `mediaplayer.exe`
3. Check "Always use this app"

---

## Troubleshooting

**"cannot find ffmpeg"** — make sure `FFMPEG_DIR` is set correctly and points to a directory containing `include/` and `lib/` subdirectories.

**No audio** — check that your default audio device is working. The player uses `rodio` which outputs to the system default device.

**Black screen / crash on some files** — try a different FFmpeg build. The gyan.dev "full" build includes more codecs than the "essentials" build.

---

## Architecture

```
src/
  main.rs      — entry point, eframe setup
  app.rs       — MediaPlayerApp: wires UI + player + drag-and-drop
  player.rs    — PlayerCommand / PlayerEvent enums, shared PlaybackState
  decoder.rs   — FFmpeg decode loop + rodio audio, runs on background thread
  ui.rs        — All egui rendering: video frame, control bar, seek bar
```

The decode loop runs on a dedicated thread and communicates via crossbeam channels.
Video frames are uploaded to a GPU texture each frame. Audio goes through rodio's default device.
