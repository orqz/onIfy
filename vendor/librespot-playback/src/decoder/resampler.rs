//! Streaming windowed-sinc resampler, so local files recorded at any sample
//! rate play through the 44.1 kHz pipeline. Added by onIfy.

use std::f64::consts::PI;

use crate::NUM_CHANNELS;

const CHANNELS: usize = NUM_CHANNELS as usize;
/// Zero crossings of the sinc kept on each side of its centre.
const ZERO_CROSSINGS: f64 = 16.0;
/// Kernel table entries per input frame; lookups interpolate between them.
const TABLE_STEPS: usize = 256;

pub struct Resampler {
    /// Input frames advanced per output frame.
    step: f64,
    /// Kernel half-width, in input frames.
    half_width: usize,
    /// The kernel from its centre outwards, every 1/TABLE_STEPS input frames.
    table: Vec<f64>,
    /// Buffered input, interleaved.
    input: Vec<f64>,
    /// Where the next output frame falls, in input frames into `input`.
    position: f64,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let step = from_rate as f64 / to_rate as f64;
        // Keep everything above the lower of the two Nyquist frequencies out,
        // with a little room for the filter's transition band.
        let cutoff = (to_rate as f64 / from_rate as f64).min(1.0) * 0.97;
        let half_width = (ZERO_CROSSINGS / cutoff).ceil() as usize;
        let table = (0..=half_width * TABLE_STEPS + 1)
            .map(|i| {
                let x = i as f64 / TABLE_STEPS as f64;
                if x >= half_width as f64 {
                    return 0.0;
                }
                let t = PI * cutoff * x;
                let sinc = if t == 0.0 { 1.0 } else { t.sin() / t };
                // Blackman window over the kernel's width.
                let w = PI * x / half_width as f64;
                let window = 0.42 + 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
                cutoff * sinc * window
            })
            .collect();
        let mut resampler = Self {
            step,
            half_width,
            table,
            input: Vec::new(),
            position: 0.0,
        };
        resampler.reset();
        resampler
    }

    /// Forgets buffered audio, e.g. after a seek.
    pub fn reset(&mut self) {
        // Silence ahead of the first frame, so output starts exactly at input frame 0.
        self.input.clear();
        self.input.resize(self.half_width * CHANNELS, 0.0);
        self.position = self.half_width as f64;
    }

    fn kernel(&self, distance: f64) -> f64 {
        let x = distance.abs() * TABLE_STEPS as f64;
        let i = x as usize;
        match (self.table.get(i), self.table.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * (x - i as f64),
            _ => 0.0,
        }
    }

    /// Takes interleaved stereo input and returns every output frame it can
    /// produce so far; the last few input frames wait for the next call.
    pub fn process(&mut self, samples: &[f64]) -> Vec<f64> {
        self.input.extend_from_slice(samples);
        let frames = self.input.len() / CHANNELS;
        let half_width = self.half_width as isize;
        let expected = (samples.len() / CHANNELS) as f64 / self.step;
        let mut out = Vec::with_capacity((expected as usize + 1) * CHANNELS);

        // Each output frame needs `half_width` input frames on both sides.
        while self.position + (self.half_width as f64) < frames as f64 {
            let centre = self.position.floor() as isize;
            let mut acc = [0.0; CHANNELS];
            for k in (centre - half_width + 1)..=(centre + half_width) {
                let weight = self.kernel(k as f64 - self.position);
                let frame = &self.input[k as usize * CHANNELS..][..CHANNELS];
                for (a, s) in acc.iter_mut().zip(frame) {
                    *a += s * weight;
                }
            }
            out.extend_from_slice(&acc);
            self.position += self.step;
        }

        // Drop input that no future output frame reaches.
        let consumed = (self.position.floor() as usize).saturating_sub(self.half_width);
        if consumed > 0 {
            self.input.drain(..consumed * CHANNELS);
            self.position -= consumed as f64;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 kHz tone at 48 kHz comes out as a 1 kHz tone at 44.1 kHz, at the
    /// same level and length.
    #[test]
    fn keeps_tone_level_and_length() {
        let (from, to) = (48_000, 44_100);
        let input: Vec<f64> = (0..from)
            .flat_map(|i| {
                let s = (2.0 * PI * 1000.0 * i as f64 / from as f64).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut resampler = Resampler::new(from, to);
        let out: Vec<f64> = input.chunks(2304).flat_map(|c| resampler.process(c)).collect();
        let frames = out.len() / CHANNELS;
        assert!((frames as i64 - to as i64).abs() < 40, "{frames} frames");

        // Compare against an ideal 1 kHz tone at 44.1 kHz, away from the edges.
        let mut worst: f64 = 0.0;
        for i in 1000..frames - 1000 {
            let ideal = (2.0 * PI * 1000.0 * i as f64 / to as f64).sin() * 0.5;
            worst = worst.max((out[i * CHANNELS] - ideal).abs());
        }
        assert!(worst < 1e-3, "error {worst}");
    }
}
