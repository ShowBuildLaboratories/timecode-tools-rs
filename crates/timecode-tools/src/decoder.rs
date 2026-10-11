use crate::label::{frames_per_day, Label};
/// A decoded frame. `sample_index` is one-based and points at the sample on
/// which the frame's final edge was detected (the Dart reference convention):
/// for a block starting at device index `s`, the first processed sample is
/// `s + 1`.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub label: Label,
    pub sample_index: u64,
}
impl PartialEq for Frame {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label
    }
}
impl Eq for Frame {}
#[derive(Clone, Copy, Debug)]
pub struct DecodedFrame {
    pub frame: Frame,
    pub locked: bool,
    pub estimated_fps: Option<u32>,
}
pub struct Decoder {
    pub sample_rate: u32,
    pub sample_index: u64,
    carry: Option<u8>,
    peak: f64,
    polarity: Option<bool>,
    edge: Option<u64>,
    period: Option<f64>,
    half: Option<f64>,
    bootstrap: Vec<u64>,
    bits: Vec<u8>,
    last: Option<Frame>,
    last_fps: u32,
    contiguous: u32,
    fps: Option<u32>,
    max_frame: i32,
}
impl Decoder {
    pub fn new(sample_rate: u32) -> Self {
        assert!(sample_rate > 0);
        Self {
            sample_rate,
            sample_index: 0,
            carry: None,
            peak: 0.,
            polarity: None,
            edge: None,
            period: None,
            half: None,
            bootstrap: Vec::new(),
            bits: Vec::new(),
            last: None,
            last_fps: 30,
            contiguous: 0,
            fps: None,
            max_frame: -1,
        }
    }
    pub fn locked(&self) -> bool {
        self.contiguous >= 4
    }
    pub fn estimated_fps(&self) -> Option<u32> {
        self.fps
    }
    pub fn signal_level(&self) -> f64 {
        (self.peak / 32768.).clamp(0., 1.)
    }
    /// Reset decoding after lost samples without renumbering the device timeline.
    pub fn gap(&mut self, next_sample: u64) {
        *self = Self::new(self.sample_rate);
        self.sample_index = next_sample;
    }
    pub fn feed_at(&mut self, first_sample: u64, pcm: &[i16]) -> Vec<DecodedFrame> {
        if self.sample_index != first_sample {
            self.gap(first_sample);
        }
        let mut out = Vec::new();
        for &s in pcm {
            self.sample(s, &mut out);
        }
        out
    }
    /// Streaming little-endian PCM16 bytes. An odd trailing byte is carried
    /// over to the next call. Do not mix with [`Decoder::feed_at`] in one stream.
    pub fn feed_bytes(&mut self, pcm: &[u8]) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        for &b in pcm {
            if let Some(lo) = self.carry.take() {
                self.sample(i16::from_le_bytes([lo, b]), &mut out);
            } else {
                self.carry = Some(b);
            }
        }
        out
    }
    fn sample(&mut self, s: i16, out: &mut Vec<DecodedFrame>) {
        self.sample_index += 1;
        let abs = f64::from(s).abs();
        self.peak = if abs > self.peak {
            abs
        } else {
            self.peak * 0.999
        };
        let threshold = 400_f64.max(self.peak * 0.1);
        let was = self.polarity;
        if f64::from(s) > threshold {
            self.polarity = Some(true);
        } else if f64::from(s) < -threshold {
            self.polarity = Some(false);
        }
        if self.polarity.is_none() {
            return;
        }
        let Some(last) = self.edge else {
            self.edge = Some(self.sample_index);
            return;
        };
        if was.is_some() && was != self.polarity {
            self.edge = Some(self.sample_index);
            let interval = self.sample_index - last;
            if self.period.is_none() {
                self.bootstrap.push(interval);
                if self.bootstrap.len() < 6 {
                    return;
                }
                let min = *self.bootstrap.iter().min().unwrap() as f64;
                let max = *self.bootstrap.iter().max().unwrap() as f64;
                let both = max >= min * 1.5;
                if !both && self.bootstrap.len() < 32 {
                    return;
                }
                self.period = Some(if both {
                    (min * 2. + max) / 2.
                } else {
                    min * 2.
                });
                for i in std::mem::take(&mut self.bootstrap) {
                    self.classify(i as f64, out);
                }
            } else {
                self.classify(interval as f64, out);
            }
        }
    }
    fn classify(&mut self, i: f64, out: &mut Vec<DecodedFrame>) {
        let (span, bit) = if i < self.period.unwrap() * 0.75 {
            if let Some(h) = self.half.take() {
                (h + i, 1)
            } else {
                self.half = Some(i);
                return;
            }
        } else {
            self.half = None;
            (i, 0)
        };
        self.period = Some(self.period.unwrap() * 0.85 + span * 0.15);
        self.bits.push(bit);
        if self.bits.len() > 80 {
            self.bits.remove(0);
        }
        if self.bits.len() != 80
            || self.bits[64..] != [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1]
        {
            return;
        }
        let bcd = |s: usize, n: usize| -> u8 { (0..n).map(|k| self.bits[s + k] << k).sum() };
        let label = Label {
            frames: bcd(8, 2) * 10 + bcd(0, 4),
            seconds: bcd(24, 3) * 10 + bcd(16, 4),
            minutes: bcd(40, 3) * 10 + bcd(32, 4),
            hours: bcd(56, 2) * 10 + bcd(48, 4),
            drop_frame: self.bits[10] == 1,
        };
        if label.frames > 39 || label.seconds > 59 || label.minutes > 59 || label.hours > 29 {
            return;
        }
        self.max_frame = self.max_frame.max(i32::from(label.frames));
        let rate = f64::from(self.sample_rate) / self.period.unwrap() / 80.;
        let matched = if (rate - 24.).abs() < 0.75 {
            Some(24)
        } else if (rate - 25.).abs() < 0.75 {
            Some(25)
        } else if (rate - 30.).abs() < 0.9 {
            Some(30)
        } else {
            None
        };
        if let Some(f) = matched {
            if f as i32 > self.max_frame {
                self.fps = Some(f);
            }
        }
        let guess = [24, 25, 30]
            .into_iter()
            .min_by(|a, b| {
                (rate - f64::from(*a))
                    .abs()
                    .total_cmp(&(rate - f64::from(*b)).abs())
            })
            .unwrap();
        let total = label.frame_index(guess);
        self.contiguous = if let Some(prev) = self.last {
            let p = prev.label.frame_index(self.last_fps);
            if total == p + 1
                || (total == 0 && p == frames_per_day(self.last_fps, prev.label.drop_frame) - 1)
            {
                self.contiguous + 1
            } else {
                1
            }
        } else {
            1
        };
        let frame = Frame {
            label,
            sample_index: self.sample_index,
        };
        self.last = Some(frame);
        self.last_fps = guess;
        out.push(DecodedFrame {
            frame,
            locked: self.locked(),
            estimated_fps: self.fps,
        });
    }
}
/// Converts a float sample to PCM16: clamp to [-1, 1], scale by 32767 and
/// round half away from zero.
pub fn pcm16(sample: f32) -> i16 {
    (sample.clamp(-1., 1.) * 32767.).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_conversion_clamps_and_rounds() {
        assert_eq!(pcm16(2.), 32767);
        assert_eq!(pcm16(-2.), -32767);
        assert_eq!(pcm16(0.5), 16384);
        assert_eq!(pcm16(-0.5), -16384);
    }
    #[test]
    fn dart_bcd_gate_rejects_out_of_range() {
        let mut d = Decoder::new(48000);
        d.period = Some(24.);
        let bits = crate::synth::frame_bits(Label {
            hours: 30,
            minutes: 0,
            seconds: 0,
            frames: 0,
            drop_frame: false,
        });
        let mut out = Vec::new();
        for bit in bits {
            if bit == 1 {
                d.classify(12., &mut out);
                d.classify(12., &mut out);
            } else {
                d.classify(24., &mut out);
            }
        }
        assert!(out.is_empty());
    }
}
