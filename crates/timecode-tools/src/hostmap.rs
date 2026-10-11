use crate::clock::{regression, Fit};
use std::collections::VecDeque;
#[derive(Clone, Copy, Debug)]
pub struct IoStamp {
    pub first_sample: u64,
    pub host_ns: u64,
    pub frames: u32,
    pub discontinuity: bool,
}
/// Ten seconds of IO-cycle stamps. x is device seconds, y engine seconds;
/// origins are subtracted before conversion to preserve large integer epochs.
pub struct HostMap {
    pub sample_rate: u32,
    points: VecDeque<(f64, f64)>,
    origin: Option<(u64, u64)>,
    expected: Option<u64>,
    last_host_ns: Option<u64>,
    fit: Option<Fit>,
    pub resets: u64,
}
impl HostMap {
    pub fn new(sample_rate: u32) -> Self {
        assert!(sample_rate > 0);
        Self {
            sample_rate,
            points: VecDeque::new(),
            origin: None,
            expected: None,
            last_host_ns: None,
            fit: None,
            resets: 0,
        }
    }
    pub fn reset(&mut self) {
        self.points.clear();
        self.origin = None;
        self.expected = None;
        self.last_host_ns = None;
        self.fit = None;
        self.resets += 1;
    }
    pub fn add_stamp(&mut self, stamp: IoStamp) -> bool {
        let reset = stamp.discontinuity
            || self.expected.is_some_and(|s| s != stamp.first_sample)
            || self.last_host_ns.is_some_and(|h| stamp.host_ns <= h);
        if reset {
            self.reset();
        }
        let (s, h) = *self
            .origin
            .get_or_insert((stamp.first_sample, stamp.host_ns));
        let x = (stamp.first_sample - s) as f64 / f64::from(self.sample_rate);
        let y = (stamp.host_ns - h) as f64 / 1e9;
        self.points.push_back((x, y));
        while self.points.len() > 2 && x - self.points.front().unwrap().0 > 10. {
            self.points.pop_front();
        }
        self.fit = regression(&self.points);
        self.expected = Some(stamp.first_sample + u64::from(stamp.frames));
        self.last_host_ns = Some(stamp.host_ns);
        reset
    }
    pub fn device_ppm(&self) -> Option<f64> {
        self.fit.map(|f| (1. / f.b - 1.) * 1e6)
    }
    pub fn jitter_rms_ns(&self) -> Option<f64> {
        self.fit.map(|f| f.residual * 1e9)
    }
    /// host(s) = h0 + k*(s-s0), k in ns/sample.
    pub fn mapping(&self) -> Option<(f64, f64, f64)> {
        let (s, h) = self.origin?;
        let f = self.fit?;
        Some((
            s as f64,
            h as f64 + f.a * 1e9,
            f.b * 1e9 / f64::from(self.sample_rate),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_reset_and_large_origins() {
        let mut h = HostMap::new(48000);
        for i in 0..20 {
            h.add_stamp(IoStamp {
                first_sample: 4_800_000_000 + i * 480,
                host_ns: 100_000_000_000_000 + i * 10_000_000,
                frames: 480,
                discontinuity: false,
            });
        }
        assert!(h.device_ppm().unwrap().abs() < 1e-6);
        assert!(h.jitter_rms_ns().unwrap() < 1.);
        h.reset();
        assert!(h.mapping().is_none());
    }
}
