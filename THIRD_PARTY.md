# Audio compression dependencies

ChemClient 1.2.0 uses shine-rs 0.1.5 (LGPL-2.0) for MP3 encoding.
Upstream: https://github.com/wshon/shine-rs ; exact registry revision is pinned in Cargo.lock.
The upstream crate source and license are available with cargo vendor --locked.
No changes to the encoder crate are included in this project.

MP3 decoding uses this repository's third_party/nanomp3 implementation; its source
and license are distributed in that directory. Application conversion logic lives
in src/transcode.rs. Codec PCM windows stay bounded; output is written to disk.
