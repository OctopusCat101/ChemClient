# ChemClient

AstroBox API 4 / WASI 2 plugin for NoirMusic: local MP3, NetEase search, QR login,
playlists, direct native-module import and library management. No watch quick-app
is needed. The plugin uses the native NoirMusic library.

Version 1.4.0 adds optional local artwork/LRC and automatic NetEase artwork/lyrics.
Artwork has one fixed 96x96 wearable resolution, independently of audio quality.
MP3 compression and optional SBC preconversion use streaming anti-alias FIR
resampling; selected compression runs first and memory stays bounded by windows.

## Build

Requires AstroBox 2.2.0 with API 4 support. Install Clang/llvm-ar on PATH (or set
CC and AR), stable Rust, Python 3.11+ and
wasm-component-ld >= 0.5.27 (available in recent nightly toolchains). The builder
automatically finds a compatible linker; --component-linker accepts an explicit path.
Bindings are pinned to wit-bindgen 0.60.0, matching the Wasmtime 48 component ABI.
Run from the repository root:

    rustup target add wasm32-wasip2
    python build.py --abp

Output stays entirely inside this plugin:

    ChemClient/                 # independent repository root
    ├── index.txt              # contains: dist
    ├── .github/workflows/chemclient.yml
    ├── third_party/            # MP3/SBC codecs, format source and licenses
    ├── dist/
    │   ├── manifest.json
    │   ├── chemclient.wasm
    │   ├── icon.png
    │   └── ChemClient.abp       # optional: --abp (alias: --package)
    └── out/cargo/              # ignored compilation cache

ABP is for local installation; the plugin market reads the unpacked files.
Upload this repository as the GitHub repository root. Its index.txt
contains the single relative path dist. All build inputs are inside this directory.

To validate existing release files without compiling, run python build.py --check
from this directory. Every file in additional_files is copied from this root using
its declared relative path. The builder does not use the native module SDK.

Before public upload use --website with the actual project URL. Commit the three
distribution files (plus any additional_files) and the root index.txt; build
caches and the optional ABP are ignored. Then submit the repository raw base URL to
AstroBox-NG-Plugin-Repo following its official plugin-upload guide. No upload is
performed automatically and no repository URL is invented. Follow the official
[plugin upload guide](https://abox.run/docs/creator-tools/resource-management/plugin-upload).
The raw base URL ends at the branch, not at dist. After pushing, verify that
dist/manifest.json and its declared resources are accessible as raw files.

## Architecture

- ui.rs: views/actions; state.rs: view state; lib.rs: async event dispatch.
- transfer.rs: one pending request, 16 KiB reads, durable offsets, safe retry,
  sequential cancel, incremental CRC32; no entire-file allocation.
- protocol.rs: bounded protobuf and native import envelope.
- netease.rs: service API and streaming download to disk.

Direct importing currently targets Band 9 N66TC / 3.1.32. Building WASM and
checking firmware addresses cannot confirm the real-device handshake. NetEase
availability depends on the account and service. This release imports audio, metadata, device-ready NCV1 covers and UTF-8 LRC or
NetEase lyric JSON (including translations). Optional cloud attachment failures
leave audio importing available. Media attachments require NoirMusic code66;
SBC without attachments retains its earlier code59 minimum.

## Connecting and layout

First scan/connect the band in AstroBox Devices. ChemClient lists connected devices;
the host plugin API does not expose radio scanning or initiating a connection.
Open the Device tab, press Refresh devices, and select a connected band to handshake
with NoirMusic. Opening/reopening the UI renders immediately without starting I/O.
Reload the plugin before switching to another band.

All lifecycle/event exports use official WIT async func and the API 4 persistent
runtime. UI actions have a non-queuing guard; tabs and input remain available during
async device/dialog waits. Each pending transport request has at most one host send
in flight, including timer polls and retries. Send/registration failures are shown
instead of silently leaving a pending request. No mutex is held across an await.

After replacing a crashed API 3 release, completely exit/restart AstroBox, then
install/reopen ChemClient 1.4.0. Reopening only the plugin does not recover the old
panicked runtime.

Version 1.4.1 polls transfer acknowledgements after 25 ms, backing off to 100 ms
when the wearable is busy. It retains one bounded 16 KiB block in flight and
the existing sequential-offset/CRC/atomic-commit guarantees. Nested protobuf
packets are encoded into one buffer instead of several audio-sized copies.
Actual speed still depends on the phone, BLE link and wearable flash.

Local MP3 import automatically uses an embedded ID3v2.3/v2.4 cover when no image
is selected. An explicitly selected cover takes priority. Artwork is converted
on the phone to the same 96x96 device format; no full-song read is required.

## Compression and optional SBC predecode

Local and NetEase imports share two independent settings. First select the MP3
compression bitrate (24, 48, 80 kbps, or Original), then optionally enable SBC
predecode. Compression always runs first; SBC conversion uses its output. Original
skips MP3 compression and converts the source directly. Disabling predecode keeps
the MP3 import workflow.

SBC predecode requires NoirMusic code59 or later on Band 9. Both stages process
bounded windows and write to disk; neither loads a full song into memory. The
client resamples to 44.1 kHz with a polyphase FIR and writes a fixed-frame NSB1
file. Matching earphones receive those frames without wearable MP3 decoding or
SBC encoding; a negotiated profile mismatch uses streaming integer SBC decode
and re-encode. Pause, seeking and track changes use the normal player controls.

SBC reduces wearable computation at the cost of storage and transfer time:
the default Joint Stereo / bitpool39 stream is about 251 kbps, or 5.65 MB for
three minutes. A preceding 24 kbps compression still reduces source quality;
converting it to SBC cannot restore that quality or retain its small file size.

## Covers and lyrics

Local import offers independent optional JPG/PNG and UTF-8 LRC selectors.
NetEase imports request the song artwork and synchronized lyric/translation.
The client derives Material dark-theme roles from the whole picture, crops a
96x96 cover, adds black corners and emits opaque BGRA8888 in NCV1. This avoids
image decoding, color analysis and image animations on the wearable. No image
quality setting is tied to audio compression. Both app icons are100x100; the
plugin icon uses the same100x100 Chem brand asset.

Selected source images are bounded to8MiB,4096x4096 and32MiB decoder allocation;
lyrics are limited to96KiB. Generated sidecars are removed after success/error/
cancel without deleting user-selected originals. Audio and attachments upload
sequentially with durable acknowledgements, so a full song is never in memory.

Audio settings retain stereo at24kHz for all three compressed levels:24kbps
(about0.18MB/min),48kbps (0.36MB/min),80kbps (0.60MB/min). Original skips MP3
compression; optional SBC conversion always follows the chosen audio stage.
