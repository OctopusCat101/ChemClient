# Audio compression dependencies

ChemClient 1.4.0 uses shine-rs 0.1.5 (LGPL-2.0) for MP3 encoding.
Upstream: https://github.com/wshon/shine-rs ; exact registry revision is pinned in Cargo.lock.
The upstream crate source and license are available with cargo vendor --locked.
No changes to the encoder crate are included in this project.

MP3 decoding uses this repository's third_party/nanomp3 implementation; its source
and license are distributed in that directory. Application conversion logic lives
in src/transcode.rs. Codec PCM windows stay bounded; output is written to disk.

Optional SBC predecode uses the BlueZ SBC C library vendored under
third_party/sbc-codec/vendor (LGPL-2.1-or-later, see LICENSE-LGPL-2.1 and source headers).
Upstream: https://git.kernel.org/pub/scm/bluetooth/sbc.git/ . The bundled version
comes from NoirMusic's existing SBC source with its caller-owned static context
and freestanding build support. The client disables unused x86 CPU detection when
SIMD implementations are not compiled. Codec compilation and Rust bindings live
in third_party/sbc-codec; incremental conversion and FIR resampling are in its
src/convert.rs. Both MP3 compression and SBC preconversion share the bounded
32-tap polyphase FIR in src/resample.rs. No external executable runs during
end-user conversion.

third_party/noirmusic-format is the vendored NSB1 format implementation shared
with NoirMusic (AGPL-3.0). It is bundled so ChemClient builds independently.

Artwork preparation uses image0.25.10 (MIT OR Apache-2.0), JPEG/PNG features only.
Upstream: https://github.com/image-rs/image . Material theme quantization and role
generation uses material-colors0.4.2 (MIT OR Apache-2.0), upstream
https://github.com/Aiving/material-colors . Exact versions and transitive codecs
are pinned in Cargo.lock; their sources/licenses are available using
cargo vendor --locked. Both run in the phone/plugin; the wearable adds no image
decoder. The client supplies the bounded NCV1 BGRA image and RGB565 theme roles.
