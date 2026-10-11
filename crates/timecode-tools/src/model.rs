use crate::{
    clock::{Clock, Fit},
    decoder::DecodedFrame,
    hostmap::{HostMap, IoStamp},
    label::frames_per_day,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Off,
    Unsupported,
    DeviceMissing,
    DeviceError,
    NoSignal,
    Acquiring,
    Locked,
    Freewheel,
}
#[derive(Clone, Copy, Debug)]
pub struct Rate {
    pub fps: f64,
    pub ambiguous: bool,
}
pub fn resolve_rate(measured: f64, nominal: u32, df: bool, project_rate: f64) -> Rate {
    let integer = f64::from(nominal);
    let fractional = integer * 1000. / 1001.;
    if df {
        return Rate {
            fps: fractional,
            ambiguous: false,
        };
    }
    let a = (measured / integer - 1.).abs() <= 500e-6;
    let b = (measured / fractional - 1.).abs() <= 500e-6;
    let ambiguous = a == b;
    let fps = match (a, b) {
        (true, false) => integer,
        (false, true) => fractional,
        _ => {
            if (project_rate - fractional).abs() < (project_rate - integer).abs() {
                fractional
            } else {
                integer
            }
        }
    };
    Rate { fps, ambiguous }
}
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub a: Option<f64>,
    pub b: Option<f64>,
    pub state: State,
    pub epoch: u64,
    pub residual: Option<f64>,
    pub device_ppm: Option<f64>,
    pub stamp_jitter_ns: Option<f64>,
    pub rate: Option<Rate>,
    pub freewheel_remaining_ns: u64,
}
impl Snapshot {
    pub fn at(self, ns: u64) -> Option<f64> {
        if matches!(self.state, State::Locked | State::Freewheel) {
            Some(self.a? + self.b? * ns as f64)
        } else {
            None
        }
    }
}
pub struct TcModel {
    pub clock: Clock,
    pub hostmap: HostMap,
    pub input_latency_ns: f64,
    pub freewheel_frames: u32,
    pub project_rate: f64,
    pub epoch: u64,
    pub rejected_frames: u64,
    pub dropouts: u64,
    state: State,
    last_ns: Option<u64>,
    anchor: Option<Fit>,
    pending: Option<(f64, f64)>,
    nominal: Option<u32>,
    df: bool,
    day_offset: f64,
    jam_refitting: bool,
}
impl TcModel {
    pub fn new(sample_rate: u32, project_rate: f64) -> Self {
        Self {
            clock: Clock::default(),
            hostmap: HostMap::new(sample_rate),
            input_latency_ns: 0.,
            freewheel_frames: 8,
            project_rate,
            epoch: 0,
            rejected_frames: 0,
            dropouts: 0,
            state: State::Off,
            last_ns: None,
            anchor: None,
            pending: None,
            nominal: None,
            df: false,
            day_offset: 0.,
            jam_refitting: false,
        }
    }
    pub fn set_input_state(&mut self, state: State) {
        assert!(!matches!(
            state,
            State::Locked | State::Freewheel | State::Acquiring
        ));
        self.state = state;
        self.clock.reset();
        self.hostmap.reset();
        self.anchor = None;
        self.pending = None;
        self.last_ns = None;
        self.nominal = None;
        self.day_offset = 0.;
        self.jam_refitting = false;
    }
    pub fn add_stamp(&mut self, stamp: IoStamp) {
        if self.hostmap.add_stamp(stamp) {
            self.clock.reset();
            self.anchor = None;
            self.pending = None;
            self.last_ns = None;
            self.state = State::NoSignal;
            self.day_offset = 0.;
            self.jam_refitting = false;
        }
    }
    pub fn observe(&mut self, event: DecodedFrame, now_ns: u64) {
        if matches!(
            self.state,
            State::Off | State::Unsupported | State::DeviceMissing | State::DeviceError
        ) {
            return;
        }
        // Expire a stopped source even when the caller did not poll snapshots.
        self.snapshot(now_ns);
        let Some(fps) = event.estimated_fps else {
            return;
        };
        self.nominal = Some(fps);
        self.df = event.frame.label.drop_frame;
        let day = frames_per_day(fps, self.df);
        let t = event.frame.sample_index as f64 / f64::from(self.hostmap.sample_rate);
        // A decoded frame X completes exactly where frame X+1 begins.
        let raw = (event.frame.label.frame_index(fps) + 1).rem_euclid(day) as f64;
        let fit = self.anchor.or(self.clock.raw_fit());
        let mut value = raw + self.day_offset;
        if let Some(f) = fit {
            let predicted = f.at(t);
            // Only a near-exact day jump is midnight. A real relocation of
            // more than half a day must not be folded into the wrong day.
            let diff = value - predicted;
            if (diff + day as f64).abs() <= 5. {
                value += day as f64;
                self.day_offset += day as f64;
            } else if (diff - day as f64).abs() <= 5. {
                value -= day as f64;
                self.day_offset -= day as f64;
            }
            if (value - predicted).abs() > 5. {
                if let Some((pt, pc)) = self.pending.take() {
                    if (value - pc - 1.).abs() < 1e-6 && t > pt && ((t - pt) * f.b - 1.).abs() < 0.5
                    {
                        self.epoch += 1;
                        self.jam_refitting = true;
                        self.clock.reset();
                        self.clock.add_observation(pt, pc, None, now_ns);
                        self.anchor = Some(Fit {
                            a: value - f.b * t,
                            b: f.b,
                            residual: f.residual,
                        });
                    } else {
                        self.rejected_frames += 1;
                        self.pending = Some((t, value));
                        return;
                    }
                } else {
                    self.pending = Some((t, value));
                    return;
                }
            } else if self.pending.take().is_some() {
                self.rejected_frames += 1;
            }
        }
        self.clock.add_observation(t, value, None, now_ns);
        self.last_ns = Some(now_ns);
        if self.clock.snapshot(now_ns).usable && (event.locked || self.anchor.is_some()) {
            self.anchor = self.clock.raw_fit();
            self.jam_refitting = false;
            self.state = State::Locked;
        } else if self.jam_refitting && self.anchor.is_some() {
            self.state = State::Locked;
        } else {
            self.state = State::Acquiring;
            self.anchor = None;
        }
    }
    pub fn snapshot(&mut self, now_ns: u64) -> Snapshot {
        let composed = self.anchor.and_then(|f| {
            self.hostmap.mapping().map(|(s, h, k)| {
                let b = f.b / f64::from(self.hostmap.sample_rate) / k;
                Fit {
                    a: f.a
                        + f.b * s / f64::from(self.hostmap.sample_rate)
                        + b * (self.input_latency_ns - h),
                    b,
                    residual: f.residual,
                }
            })
        });
        if self.anchor.is_none() && self.state == State::Acquiring {
            if let (Some(last), Some(fps)) = (self.last_ns, self.nominal) {
                if now_ns.saturating_sub(last) as f64 > 2e9 / f64::from(fps) {
                    self.state = State::NoSignal;
                    self.clock.reset();
                    self.pending = None;
                }
            }
        }
        let mut remaining = 0;
        if let (Some(last), Some(f)) = (self.last_ns, composed) {
            let age = now_ns.saturating_sub(last) as f64;
            let period = 1. / f.b;
            // Eight-frame window is measured from the last accepted frame;
            // the first two periods are the loss-detection grace.
            let end = f64::from(self.freewheel_frames) * period;
            remaining = (end - age).max(0.) as u64;
            if age > 2. * period {
                if age <= end {
                    if self.state != State::Freewheel {
                        self.dropouts += 1;
                    }
                    self.state = State::Freewheel;
                } else {
                    self.state = State::NoSignal;
                    self.anchor = None;
                    self.clock.reset();
                    self.pending = None;
                    self.jam_refitting = false;
                }
            }
        }
        if self.state == State::Locked && composed.is_none() {
            self.state = State::Acquiring;
        }
        let valid = matches!(self.state, State::Locked | State::Freewheel) && composed.is_some();
        let f = composed.filter(|_| valid);
        Snapshot {
            a: f.map(|f| f.a),
            b: f.map(|f| f.b),
            state: self.state,
            epoch: self.epoch,
            residual: composed.map(|f| f.residual),
            device_ppm: self.hostmap.device_ppm(),
            stamp_jitter_ns: self.hostmap.jitter_rms_ns(),
            rate: composed.and_then(|f| {
                self.nominal
                    .map(|n| resolve_rate(f.b * 1e9, n, self.df, self.project_rate))
            }),
            freewheel_remaining_ns: remaining,
        }
    }
}

/// Rounds a fractional frame position to the nearest frame. Exact half-frame
/// ties go to the later frame (also for negative values).
pub fn nearest_frame(frame: f64) -> i64 {
    (frame + 0.5).floor() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_frame_ties_are_later() {
        assert_eq!(nearest_frame(2.5), 3);
        assert_eq!(nearest_frame(-2.5), -2);
        assert_eq!(nearest_frame(2.49), 2);
    }
    #[test]
    fn input_states_never_publish_position() {
        let mut m = TcModel::new(48000, 25.);
        for s in [
            State::Off,
            State::Unsupported,
            State::DeviceMissing,
            State::DeviceError,
            State::NoSignal,
        ] {
            m.set_input_state(s);
            let snap = m.snapshot(0);
            assert_eq!(snap.state, s);
            assert!(snap.at(0).is_none());
        }
    }
}
