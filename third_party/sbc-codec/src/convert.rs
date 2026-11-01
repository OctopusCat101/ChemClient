//! Bounded MP3 -> polyphase FIR -> SBC conversion. Only codec windows are resident.
use crate::{Encoder, resample::Resampler};
use nanomp3::{Decoder, MAX_SAMPLES_PER_FRAME};
use noirmusic_format::{HEADER_SIZE, Header, Profile, SAMPLE_RATE};
use std::io::{Read, Seek, SeekFrom, Write};

pub struct Converter<R, W> {
    source: R,
    output: W,
    decoder: Box<Decoder>,
    encoder: Encoder,
    input: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    finished: bool,
    pcm: Box<[i16; MAX_SAMPLES_PER_FRAME]>,
    resampler: Resampler<SAMPLE_RATE>,
    converted: Vec<i16>,
    pending: [i16; 256],
    pending_len: usize,
    encoded: Vec<u8>,
    samples: u64,
    read: u64,
}

impl<R: Read + Seek, W: Write + Seek> Converter<R, W> {
    pub fn new(mut source: R, mut output: W) -> Result<Self, String> {
        let mut tag = [0; 10];
        let n = source.read(&mut tag).map_err(|e| e.to_string())?;
        let skip = if n == 10 && &tag[..3] == b"ID3" && tag[6..].iter().all(|b| b & 128 == 0) {
            10 + tag[6..].iter().fold(0u64, |v, b| (v << 7) | u64::from(*b))
                + if tag[5] & 16 != 0 { 10 } else { 0 }
        } else {
            0
        };
        source
            .seek(SeekFrom::Start(skip))
            .map_err(|e| e.to_string())?;
        output
            .write_all(&[0; HEADER_SIZE])
            .map_err(|e| e.to_string())?;
        Ok(Self {
            source,
            output,
            decoder: Box::new(Decoder::new()),
            encoder: Encoder::new(Profile::DEFAULT.bitpool, Profile::DEFAULT.mode)?,
            input: vec![0; 32768],
            start: 0,
            end: 0,
            eof: false,
            finished: false,
            pcm: Box::new([0; MAX_SAMPLES_PER_FRAME]),
            resampler: Resampler::new(),
            converted: Vec::with_capacity(16384),
            pending: [0; 256],
            pending_len: 0,
            encoded: Vec::with_capacity(8192),
            samples: 0,
            read: skip,
        })
    }

    pub fn bytes_read(&self) -> u64 {
        self.read
    }
    pub fn header(&self) -> Header {
        Header {
            profile: Profile::DEFAULT,
            samples: self.samples,
        }
    }

    /// Decodes at most one MP3 frame. The event driver bounds successive steps.
    pub fn step(&mut self) -> Result<bool, String> {
        if self.finished {
            return Ok(true);
        }
        if !self.eof && self.end - self.start < 8192 {
            self.input.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            let n = self
                .source
                .read(&mut self.input[self.end..])
                .map_err(|e| e.to_string())?;
            self.end += n;
            self.read += n as u64;
            self.eof = n == 0;
        }
        if self.start == self.end && self.eof {
            return self.finish();
        }
        let (used, info) = self
            .decoder
            .decode(&self.input[self.start..self.end], self.pcm.as_mut());
        if used > self.end - self.start {
            return Err("Invalid MP3 consumption".into());
        }
        self.start += used;
        let Some(info) = info else {
            if used == 0 {
                if self.eof {
                    return self.finish();
                }
                if self.end - self.start >= 8192 {
                    return Err("Invalid MP3 stream".into());
                }
            }
            return Ok(false);
        };
        let channels = info.channels.num() as usize;
        if !(8000..=48000).contains(&info.sample_rate)
            || !(1..=2).contains(&channels)
            || info.samples_produced > self.pcm.len() / channels
        {
            return Err("Unsupported MP3 stream".into());
        }
        self.resampler.configure(info.sample_rate);
        self.converted.clear();
        for frame in self.pcm[..info.samples_produced * channels].chunks_exact(channels) {
            self.resampler
                .push([frame[0], frame[channels - 1]], &mut self.converted);
        }
        self.samples += self.converted.len() as u64 / 2;
        self.encoded.clear();
        for sample in &self.converted {
            self.pending[self.pending_len] = *sample;
            self.pending_len += 1;
            if self.pending_len == 256 {
                let mut frame = [0; 119];
                let n = self.encoder.encode(&self.pending, &mut frame)?;
                self.encoded.extend_from_slice(&frame[..n]);
                self.pending_len = 0;
            }
        }
        if self.header().file_bytes() > 128 * 1024 * 1024 {
            return Err("SBC 文件超过 128 MiB".into());
        }
        self.output
            .write_all(&self.encoded)
            .map_err(|e| e.to_string())?;
        Ok(false)
    }

    fn finish(&mut self) -> Result<bool, String> {
        if self.samples == 0 {
            return Err("音频未产生可播放数据".into());
        }
        if self.pending_len != 0 {
            self.pending[self.pending_len..].fill(0);
            let mut frame = [0; 119];
            let n = self.encoder.encode(&self.pending, &mut frame)?;
            self.output
                .write_all(&frame[..n])
                .map_err(|e| e.to_string())?;
            self.pending_len = 0;
        }
        self.output
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        self.output
            .write_all(&self.header().encode())
            .map_err(|e| e.to_string())?;
        self.output.flush().map_err(|e| e.to_string())?;
        self.finished = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fir_keeps_phase_and_dc_gain_across_arbitrary_chunks() {
        for rate in [8000, 24000, 44100, 48000] {
            let mut r = Resampler::<SAMPLE_RATE>::new();
            r.configure(rate);
            let mut out = Vec::new();
            for _ in 0..rate {
                r.push([12000, -12000], &mut out);
            }
            assert_eq!(out.len(), SAMPLE_RATE as usize * 2);
            assert!(out.chunks_exact(2).all(|p| p[0] == 12000 && p[1] == -12000));
        }
    }
}
