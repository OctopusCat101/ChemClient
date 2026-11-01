#![no_std]

pub const HEADER_SIZE: usize = 32;
pub const SAMPLE_RATE: u32 = 44_100;
pub const FRAME_SAMPLES: u32 = 128;
pub const DEFAULT_BITPOOL: u8 = 39;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// SBC channel mode: 2 = stereo, 3 = joint stereo.
    pub mode: u8,
    pub bitpool: u8,
}

impl Profile {
    pub const DEFAULT: Self = Self {
        mode: 3,
        bitpool: DEFAULT_BITPOOL,
    };

    pub const fn valid(self) -> bool {
        matches!(self.mode, 2 | 3) && self.bitpool >= 27 && self.bitpool <= 53
    }

    pub const fn frame_bytes(self) -> usize {
        12 + (self.mode == 3) as usize + 2 * self.bitpool as usize
    }

    pub const fn flags(self) -> u32 {
        self.bitpool as u32 | ((self.mode as u32) << 8)
    }

    pub fn from_flags(flags: u32) -> Option<Self> {
        let profile = Self {
            mode: (flags >> 8) as u8,
            bitpool: flags as u8,
        };
        (flags < 65536 && profile.valid()).then_some(profile)
    }

    pub fn matches_frame(self, frame: &[u8]) -> bool {
        frame.len() == self.frame_bytes()
            && frame[0] == 0x9c
            && frame[1] == (0xb1 | self.mode << 2)
            && frame[2] == self.bitpool
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub profile: Profile,
    /// Valid PCM sample frames; the last SBC frame may contain zero padding.
    pub samples: u64,
}

impl Header {
    pub fn frames(self) -> u32 {
        self.samples.div_ceil(u64::from(FRAME_SAMPLES)) as u32
    }

    pub fn file_bytes(self) -> u64 {
        HEADER_SIZE as u64 + u64::from(self.frames()) * self.profile.frame_bytes() as u64
    }

    pub fn duration_ms(self) -> u32 {
        (self.samples * 1000 / u64::from(SAMPLE_RATE)).min(u64::from(u32::MAX)) as u32
    }

    pub fn valid(self) -> bool {
        self.profile.valid()
            && self.samples > 0
            && self.samples <= u64::from(u32::MAX) * u64::from(FRAME_SAMPLES)
    }

    pub fn encode(self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..4].copy_from_slice(b"NSB1");
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        bytes[8..12].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        bytes[12] = self.profile.mode;
        bytes[13] = self.profile.bitpool;
        bytes[14..16].copy_from_slice(&(self.profile.frame_bytes() as u16).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.samples.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.frames().to_le_bytes());
        bytes
    }

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_SIZE
            || &bytes[..4] != b"NSB1"
            || bytes[4..8] != [1, 0, HEADER_SIZE as u8, 0]
            || u32::from_le_bytes(bytes[8..12].try_into().ok()?) != SAMPLE_RATE
            || bytes[28..32] != [0; 4]
        {
            return None;
        }
        let header = Self {
            profile: Profile {
                mode: bytes[12],
                bitpool: bytes[13],
            },
            samples: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        };
        (header.valid()
            && usize::from(u16::from_le_bytes(bytes[14..16].try_into().ok()?))
                == header.profile.frame_bytes()
            && u32::from_le_bytes(bytes[24..28].try_into().ok()?) == header.frames())
        .then_some(header)
    }

    /// Fixed-frame seek, snapped down by less than 2.91 ms. Never seek past EOF.
    pub fn seek(self, position_ms: u32) -> (u32, u32) {
        let sample = (u64::from(position_ms) * u64::from(SAMPLE_RATE) / 1000)
            .min(self.samples.saturating_sub(1));
        let frame = sample / u64::from(FRAME_SAMPLES);
        let offset = HEADER_SIZE as u64 + frame * self.profile.frame_bytes() as u64;
        let position = frame * u64::from(FRAME_SAMPLES) * 1000 / u64::from(SAMPLE_RATE);
        (offset as u32, position as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_geometry_and_seek_include_the_short_tail() {
        let header = Header {
            profile: Profile::DEFAULT,
            samples: 44_100 * 180 + 1,
        };
        assert_eq!(Header::parse(&header.encode()), Some(header));
        let (offset, position) = header.seek(123_456);
        assert!((offset as u64) < header.file_bytes());
        assert_eq!(
            (offset as usize - HEADER_SIZE) % header.profile.frame_bytes(),
            0
        );
        assert!((123_453..=123_456).contains(&position));
        assert!((header.seek(u32::MAX).0 as u64) < header.file_bytes());
    }

    #[test]
    fn reject_unsupported_or_inconsistent_headers() {
        let header = Header {
            profile: Profile::DEFAULT,
            samples: 129,
        };
        for at in [0, 4, 6, 8, 12, 13, 14, 24, 28] {
            let mut bytes = header.encode();
            bytes[at] ^= 0x80;
            assert_eq!(Header::parse(&bytes), None);
        }
        assert_eq!(Profile::from_flags(0x10327), None);
        assert_eq!(
            Profile::from_flags(Profile::DEFAULT.flags()),
            Some(Profile::DEFAULT)
        );
    }
}
