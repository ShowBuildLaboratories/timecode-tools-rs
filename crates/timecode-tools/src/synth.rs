//! Deterministic LTC (biphase-mark) test-signal generator. Eight preamble bits
//! match the Dart reference encoder. Rise time is the 10–90% time of a linear
//! ramp, default 40 us (the SMPTE 12M nominal value); set `rise_time_us = 0`
//! for the square wave the Dart encoder produces. Noise is seeded uniform white
//! noise scaled to the requested RMS SNR. [`Signal::truth`] is continuous and
//! independent of PCM quantization, decoder thresholds and decoder output.
use crate::label::Label;
#[derive(Clone, Debug)]
pub struct SynthConfig {
    pub sample_rate: u32,
    pub rate: f64,
    pub nominal_fps: u32,
    pub drop_frame: bool,
    pub start_index: i64,
    pub frame_count: usize,
    pub amplitude: f64,
    pub dc: f64,
    pub snr_db: Option<f64>,
    pub device_ppm: f64,
    pub source_ppm: f64,
    pub varispeed: f64,
    pub rise_time_us: f64,
    pub preamble_bit: u8,
    pub seed: u64,
    /// Half-open source-frame ranges replaced with silence.
    pub dropouts: Vec<std::ops::Range<usize>>,
    /// (source frame ordinal, new label index). Last relocation persists.
    pub relocates: Vec<(usize, i64)>,
    /// Isolated valid-BCD corrupt labels, leaving the true timeline unchanged.
    pub corrupt_frames: Vec<(usize, i64)>,
}
impl Default for SynthConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            rate: 25.,
            nominal_fps: 25,
            drop_frame: false,
            start_index: 0,
            frame_count: 150,
            amplitude: 20000.,
            dc: 0.,
            snr_db: None,
            device_ppm: 0.,
            source_ppm: 0.,
            varispeed: 1.,
            rise_time_us: 40.,
            preamble_bit: 1,
            seed: 7,
            dropouts: Vec::new(),
            relocates: Vec::new(),
            corrupt_frames: Vec::new(),
        }
    }
}
pub struct Signal {
    pub pcm: Vec<i16>,
    pub config: SynthConfig,
    pub frame_start_seconds: f64,
}
impl Signal {
    pub fn effective_rate(&self) -> f64 {
        self.config.rate * (1. + self.config.source_ppm * 1e-6) * self.config.varispeed
    }
    pub fn host_ns(&self, sample: u64) -> u64 {
        (sample as f64
            / (f64::from(self.config.sample_rate) * (1. + self.config.device_ppm * 1e-6))
            * 1e9)
            .round() as u64
    }
    pub fn truth(&self, host_ns: u64) -> f64 {
        let frame = (host_ns as f64 / 1e9 - self.frame_start_seconds) * self.effective_rate();
        let mut origin = self.config.start_index as f64;
        for &(at, index) in &self.config.relocates {
            if frame >= at as f64 {
                origin = index as f64 - at as f64;
            }
        }
        origin + frame
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
    }
}
pub fn frame_bits(label: Label) -> [u8; 80] {
    let mut bits = [0; 80];
    for (start, count, value) in [
        (0, 4, label.frames % 10),
        (8, 2, label.frames / 10),
        (16, 4, label.seconds % 10),
        (24, 3, label.seconds / 10),
        (32, 4, label.minutes % 10),
        (40, 3, label.minutes / 10),
        (48, 4, label.hours % 10),
        (56, 2, label.hours / 10),
    ] {
        for k in 0..count {
            bits[start + k] = (value >> k) & 1;
        }
    }
    bits[10] = u8::from(label.drop_frame);
    bits[64..].copy_from_slice(&[0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1]);
    bits
}
pub fn generate(config: SynthConfig) -> Signal {
    assert!(config.sample_rate > 0 && config.rate > 0. && config.varispeed > 0.);
    let rate = config.rate * (1. + config.source_ppm * 1e-6) * config.varispeed;
    let sr = f64::from(config.sample_rate) * (1. + config.device_ppm * 1e-6);
    let cell = sr / (rate * 80.);
    let mut levels: Vec<(f64, f64)> = Vec::new();
    let mut target: f64 = 0.;
    let mut level = 1.;
    let mut cell_out = |bit: u8| {
        level = -level;
        levels.push((target, level));
        if bit == 1 {
            target += cell / 2.;
            level = -level;
            levels.push((target, level));
            target += cell / 2.;
        } else {
            target += cell;
        }
    };
    for _ in 0..8 {
        cell_out(config.preamble_bit);
    }
    let mut index = config.start_index;
    for ordinal in 0..config.frame_count {
        if let Some(&(_, new)) = config.relocates.iter().find(|(at, _)| *at == ordinal) {
            index = new;
        }
        let encoded = config
            .corrupt_frames
            .iter()
            .find(|(at, _)| *at == ordinal)
            .map_or(index, |(_, i)| *i);
        for bit in frame_bits(Label::from_index(
            encoded,
            config.nominal_fps,
            config.drop_frame,
        )) {
            cell_out(bit);
        }
        index += 1;
    }
    for _ in 0..8 {
        cell_out(0);
    }
    let length = target.round() as usize;
    let mut pcm = Vec::with_capacity(length);
    let mut edge = 0;
    let mut rng = config.seed.max(1);
    let ramp = config.rise_time_us * 1e-6 * sr / 0.8;
    for sample in 0..length {
        while edge + 1 < levels.len() && levels[edge + 1].0.round() <= sample as f64 {
            edge += 1;
        }
        let (at, new) = levels[edge];
        let elapsed = sample as f64 - at.round();
        let shape = if ramp > 0. && edge > 0 {
            let old = levels[edge - 1].1;
            old + (new - old) * (elapsed / ramp).clamp(0., 1.)
        } else {
            new
        };
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let uniform = (rng as f64 / u64::MAX as f64) * 2. - 1.;
        let noise = config.snr_db.map_or(0., |db| {
            uniform * 3_f64.sqrt() * config.amplitude / 10_f64.powf(db / 20.)
        });
        let ordinal = ((sample as f64 / cell - 8.) / 80.).floor();
        let dropped = ordinal >= 0.
            && config
                .dropouts
                .iter()
                .any(|r| r.contains(&(ordinal as usize)));
        let value = if dropped {
            0.
        } else {
            shape * config.amplitude + config.dc + noise
        };
        pcm.push(value.round().clamp(-32768., 32767.) as i16);
    }
    Signal {
        pcm,
        frame_start_seconds: 8. / (rate * 80.),
        config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_seed_and_truth_ignores_corruption() {
        let c = SynthConfig {
            snr_db: Some(30.),
            corrupt_frames: vec![(20, 500)],
            ..Default::default()
        };
        let a = generate(c.clone());
        let b = generate(c);
        assert_eq!(a.pcm, b.pcm);
        assert!((a.truth(804_000_000) - 20.).abs() < 1e-9);
    }
}
