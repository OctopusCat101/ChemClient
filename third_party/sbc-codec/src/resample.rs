//! Streaming anti-alias FIR shared by MP3 compression and SBC preconversion.
//! All state is bounded; filter coefficients change only with the source rate.
const TAPS: usize = 32;
const PHASES: usize = 256;

pub struct Resampler<const OUTPUT_RATE: u32> {
    rate: u32,
    phase: u32,
    history: [[i16; 2]; TAPS],
    head: usize,
    initialized: bool,
    coefficients: Vec<[f64; TAPS]>,
}

impl<const OUTPUT_RATE: u32> Default for Resampler<OUTPUT_RATE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const OUTPUT_RATE: u32> Resampler<OUTPUT_RATE> {
    pub fn new() -> Self {
        assert!(OUTPUT_RATE > 0);
        Self {
            rate: 0,
            phase: 0,
            history: [[0; 2]; TAPS],
            head: 0,
            initialized: false,
            coefficients: vec![[0.0; TAPS]; PHASES],
        }
    }

    pub fn configure(&mut self, rate: u32) {
        assert!((8000..=48000).contains(&rate));
        if self.rate == rate {
            return;
        }
        self.rate = rate;
        self.phase = 0;
        self.initialized = false;
        if rate == OUTPUT_RATE {
            return;
        }
        let cutoff = (OUTPUT_RATE as f64 / rate as f64).min(1.0) * 0.94;
        for (phase, table) in self.coefficients.iter_mut().enumerate() {
            let center = 15.0 + phase as f64 / PHASES as f64;
            let mut sum = 0.0;
            for (tap, value) in table.iter_mut().enumerate() {
                let x = tap as f64 - center;
                let z = std::f64::consts::PI * cutoff * x;
                let sinc = if z.abs() < 1e-12 { 1.0 } else { z.sin() / z };
                let window =
                    0.5 - 0.5 * (2.0 * std::f64::consts::PI * tap as f64 / (TAPS - 1) as f64).cos();
                *value = sinc * cutoff * window;
                sum += *value;
            }
            for value in table {
                *value /= sum;
            }
        }
    }

    pub fn push(&mut self, pair: [i16; 2], output: &mut Vec<i16>) {
        assert!(self.rate != 0);
        if self.rate == OUTPUT_RATE {
            output.extend_from_slice(&pair);
            return;
        }
        if !self.initialized {
            self.history.fill(pair);
            self.initialized = true;
        }
        self.head = (self.head + 1) % TAPS;
        self.history[self.head] = pair;
        self.phase += OUTPUT_RATE;
        while self.phase >= self.rate {
            self.phase -= self.rate;
            let phase = (self.phase as usize * PHASES / OUTPUT_RATE as usize).min(PHASES - 1);
            let mut sum = [0.0; 2];
            for (tap, coefficient) in self.coefficients[phase].iter().enumerate() {
                let sample = self.history[(self.head + TAPS - tap) % TAPS];
                sum[0] += sample[0] as f64 * coefficient;
                sum[1] += sample[1] as f64 * coefficient;
            }
            output.push(sum[0].round().clamp(-32768.0, 32767.0) as i16);
            output.push(sum[1].round().clamp(-32768.0, 32767.0) as i16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_filter_preserves_dc_duration_and_rate_bypass() {
        for rate in [8000, 16000, 24000, 32000, 44100, 48000] {
            let mut r = Resampler::<24000>::new();
            r.configure(rate);
            let mut out = Vec::new();
            for _ in 0..rate {
                r.push([12000, -12000], &mut out);
            }
            assert_eq!(out.len(), 48000);
            assert!(out.chunks_exact(2).all(|v| v == [12000, -12000]));
        }
        let mut r = Resampler::<24000>::new();
        r.configure(24000);
        let mut out = Vec::new();
        for i in -1000i16..1000 {
            r.push([i, -i], &mut out);
        }
        assert_eq!(
            out,
            (-1000i16..1000).flat_map(|i| [i, -i]).collect::<Vec<_>>()
        );
    }

    #[test]
    fn compression_filter_rejects_alias_without_dulling_passband() {
        fn rms(frequency: f64) -> f64 {
            let mut r = Resampler::<24000>::new();
            r.configure(48000);
            let mut out = Vec::new();
            for i in 0..48000 {
                let value = (12000.0
                    * (std::f64::consts::TAU * frequency * i as f64 / 48000.0).sin())
                    as i16;
                r.push([value, value], &mut out);
            }
            let samples = &out[512..];
            (samples.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
        }
        let pass = rms(1000.0);
        let alias = rms(16000.0);
        assert!((8300.0..8700.0).contains(&pass), "passband RMS {pass}");
        assert!(
            alias / pass < 0.01,
            "alias rejection {} dB",
            20.0 * (alias / pass).log10()
        );
    }
}
