use std::ffi::c_void;

pub mod convert;
pub mod resample;

unsafe extern "C" {
    fn canopus_sbc_encoder_size() -> u32;
    fn canopus_sbc_encoder_init(
        memory: *mut c_void,
        size: u32,
        bitpool: u8,
        mode: u8,
    ) -> *mut c_void;
    fn canopus_sbc_encoder_encode(
        context: *mut c_void,
        pcm: *const i16,
        frames: u32,
        output: *mut u8,
        capacity: u32,
    ) -> i32;
}

pub struct Encoder {
    // Moving the handle never moves the allocation referenced by the C state.
    _storage: Vec<u8>,
    context: *mut c_void,
    frame_bytes: usize,
}

impl Encoder {
    pub fn new(bitpool: u8, mode: u8) -> Result<Self, String> {
        if !(27..=53).contains(&bitpool) || !matches!(mode, 2 | 3) {
            return Err("Unsupported SBC profile".into());
        }
        let size = unsafe { canopus_sbc_encoder_size() };
        let mut storage = vec![0; size as usize];
        let context =
            unsafe { canopus_sbc_encoder_init(storage.as_mut_ptr().cast(), size, bitpool, mode) };
        if context.is_null() {
            return Err("SBC initialization failed".into());
        }
        Ok(Self {
            _storage: storage,
            context,
            frame_bytes: 12 + usize::from(mode == 3) + 2 * bitpool as usize,
        })
    }

    pub fn encode(&mut self, pcm: &[i16; 256], output: &mut [u8; 119]) -> Result<usize, String> {
        let result = unsafe {
            canopus_sbc_encoder_encode(
                self.context,
                pcm.as_ptr(),
                128,
                output.as_mut_ptr(),
                output.len() as u32,
            )
        };
        if result != self.frame_bytes as i32 {
            return Err(format!("SBC encoding failed: {result}"));
        }
        Ok(result as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_profile_emits_expected_frame_header() {
        for mode in [2, 3] {
            for bitpool in [27, 39, 53] {
                let mut encoder = Encoder::new(bitpool, mode).unwrap();
                let mut frame = [0; 119];
                let n = encoder.encode(&[0; 256], &mut frame).unwrap();
                assert_eq!(n, 12 + usize::from(mode == 3) + 2 * bitpool as usize);
                assert_eq!(&frame[..3], &[0x9c, 0xb1 | mode << 2, bitpool]);
            }
        }
    }
}
