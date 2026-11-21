# ChemClient

ChemClient 1.7.35 is the Chem module manager and NoirMusic/Magic importer for API 4 / WASI 2.
Select an installed module to open its content uploader directly. The enable switch,
Delete and available Update icons share one compact row beside the module.
Normal rows show only the module name and controls. Delete uses a static trash
icon and Update a static upward arrow; Refresh, Import and Install keep text labels.
Icon buttons, images and switches have fixed min/max dimensions and explicitly
disable animation, transition and transform. Module rows reuse host element IDs
across refreshes. A fixed-width control strip always shows Update, disabled when no newer cloud
version is available, so refresh does not move any controls. Icons have accessible labels
and hover tooltips. Pending changes and failures still show a short status.
Deletion uses a confirmation dialog; no management panel expands.
NoirMusic provides local MP3, NetEase and library views in the black and pale-green
interface. Local upload starts with file selection and one Import action;
quality, cover and lyrics are grouped under the optional import settings.

The independent `com.chem.manager` bridge currently supports Band 9 / 3.1.35.
Run Chem once on the wearable. Loader build 0x26111303 processes management on
the existing resident owner loop, including when LVGL screen refresh stops.
Startup probes the resident service directly, independent of the current watchface.
Install and run Chem manually on the wearable before connecting.
Management replies use SDK ABI 2.0, matching outgoing requests.
Automatic startup is off by default and requires explicit confirmation in the
manager. Startup guards persist before module execution and observe stability
for 60 seconds. Safe mode offers Retry, Disable all and Roll back updates;
saved recovery changes take effect after reboot. Device reboot and flash behavior
still require physical validation; host and ARM simulation checks have passed.
Module downloads, upload progress and errors appear beneath their own cards.
The connection panel disappears once the module registry is connected, and
background catalog checks do not display progress cards.
Bulk file writes use a separate bounded 16 KiB buffer; CPC2 control payloads stay
limited to 512 bytes. A 12-byte ID/op poll retrieves cached replies on the receive
owner, with 25/50/100 ms backoff (250 ms when waiting on control replies) and full-request retry after one second if needed.
Writes stay ordered on the resident owner, retaining signed size/hash validation,
duplicate suppression and safe-mode guards. Request IDs are reserved durably in
ranges of 4096, and upload UI rendering is limited to once per 400 ms.
Update both the plugin and loader to use bulk delivery. Host/ARM checks do not
establish physical BLE throughput or device acceptance.
Enable/disable/remove save next-load
intent; they do not hot-unload resident code. Install the matching new chem and
NoirMusic builds, then reload this plugin. Music uses `com.chem.music`.
Native .chem installation validates the signed receipt, target and firmware, then
streams directly to Chem. Cloud refresh lists compatible modules with Install
and Update actions. cloud.json points to OctopusCat101/ChemCloud through
https://gh-proxy.com/ . Magic provides resource package import and management;
other unsupported modules show their management controls only.

Startup automatically checks the host connection, exact device identity and
resident service. When the service is unavailable or ChemCloud has a newer
installer, click Install in the module list. ChemClient opens an installed
installer or downloads and verifies it, hands it to the AstroBox queue, then
switches to it after upload succeeds. Tap Run on the wearable to start Chem.
If AstroBox's automatic local-resource installation is off, start the task from
its installation queue. The installer uses the Queue and Watchface permissions.
When updating an active Chem build, reboot the wearable and run the new installer.
If autostart is enabled on an older build, turn it off in the wearable's Chem
manager before rebooting.
Cloud modules download, verify and import after one Install or Update click.
No device page or manual module-connection sequence is needed.

Music task cards stay inside NoirMusic's uploader views, including completed jobs;
returning to the module homepage never displays them. Search results keep progress
beneath the matching song, with one fallback card inside NoirMusic if that song is
not in the current result set. Native UI element identities are scoped by page and
module/song/package ID so inserting progress, refreshing lists or navigating does
not reuse another card's nodes.

Cloud requests use nonblocking WASI HTTP polling, bounded ticks, a 10-second
connect timeout, 20 seconds without data and a two-minute total deadline. The UI
remains responsive while waiting. Both response futures and body streams subscribe
and poll readiness before reads. Known-length downloads complete on the last byte,
with catalog/HTTP length and SHA-256 checks; completion renders before verification
and module delivery. The download timer stops after the completion handoff.

Optional local artwork/LRC and automatic NetEase artwork/lyrics are supported.
NetEase HTTP waits use the API 4 runtime's native WASI 0.3 async clock, so they
do not depend on Astro timer callbacks waking a different component task.
Ready data is drained in bounded turns without a fixed delay between batches.
Request/header/completion timings are logged as `[chem:http:N]` without cookies
or URL query values. The native host API remains Level 4; Rust std and HTTP
streams still use WASI 2, alongside the async clock already provided by Astro.
The runtime test builds the production HTTP source into a WASM component and
checks delayed responses, concurrent heartbeat, streaming POST/cookies, 16 MiB
download, redirects, chunked/truncated bodies, limits, keepalive and cancellation:

    python tests/check_network_runtime.py --wasmtime PATH_TO_WASMTIME_48
    python tests/check_network_runtime.py --wasmtime PATH_TO_WASMTIME_48 --netease

The second command additionally times four anonymous EAPI calls. It does not
read saved account credentials. Results are runtime/network checks, not a
physical Astro UI or mobile network acceptance test.

Version 1.5.0 generates real 160x160 wearable artwork, independently of audio
quality. Importing covers requires NoirMusic 0.4.50 / code82 or later.
Only 160x160 covers are supported; previously imported 96x96 covers must be reimported.
MP3 compression and optional SBC preconversion use streaming anti-alias FIR
resampling; selected compression runs first and memory stays bounded by windows.

## Build

Requires AstroBox 2.2.6 with API 4 queue support. Install Clang/llvm-ar on PATH (or set
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
    ├── third_party/            # MP3/SBC codecs, format source and licenses
    ├── dist/
    │   ├── manifest.json
    │   ├── chemclient.wasm
    │   ├── icon.png
    │   └── ChemClient.abp       # optional: --abp (alias: --package)
    └── out/cargo/              # ignored compilation cache

ABP is for local installation; the plugin market reads the unpacked files.
This personal project has no GitHub Actions workflows; builds and checks run locally.
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
- connection.rs: automatic host connection checks and service discovery.
- transfer.rs: one pending request, 16 KiB reads, durable offsets, safe retry,
  sequential cancel, incremental CRC32; no entire-file allocation.
- protocol.rs: bounded protobuf and native import envelope.
- netease.rs: service API and streaming download to disk.

Direct importing currently targets Band 9 N66TC / 3.1.35. Building WASM and
checking firmware addresses cannot confirm the real-device handshake. NetEase
availability depends on the account and service. This release imports audio, metadata, device-ready NCV1 covers and UTF-8 LRC or
NetEase lyric JSON (including translations). Optional cloud attachment failures
leave audio importing available. Lyrics require NoirMusic code66; 160x160 covers
require code82. SBC without attachments retains its earlier code59 minimum.

## Connecting and layout

Connect the band in Astro first. At plugin startup, ChemClient automatically reads
Astro's connected devices, syncs the Chem module list, and prepares the NoirMusic
import service when that module is enabled and loaded. No separate device or
manager connection is needed. Device connection events also recheck automatically;
if no band or service is available, the UI shows one message and Refresh.
Refresh repeats the same automatic flow without interrupting an active import.
With multiple connected bands, the current band is preferred; otherwise the band
with the first address in sorted order is used. Reopening the UI renders immediately.

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

Connection revision 1.7.23 / loader 0x26111303 starts the local service handshake
without waiting for the cloud catalog or DeviceInfo. The catalog still refreshes
from the server each time, and signed module import waits for exact identity
validation. A CMS2 query returns device, boot and up to four module rows in one
reply; further rows remain paginated. Ordinary queries retain their original
wire size. CSQ1 repairs a lost client sequence without resetting the wearable's
mutation replay protection. API calls have cooperative deadlines; failed service
and music handshakes retry independently. Idle session refreshes keep controls
available and yield to user operations; reconnects invalidate old requests.
The wearable joins an existing resident owner directly, preserving screen-off
startup, Flash checks, safe mode and stability observation.
