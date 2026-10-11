use std::collections::VecDeque;
#[derive(Clone, Copy, Debug)]
pub struct Fit {
    pub a: f64,
    pub b: f64,
    pub residual: f64,
}
impl Fit {
    pub fn at(self, t: f64) -> f64 {
        self.a + self.b * t
    }
}
/// Summation order intentionally matches Dart's centered least squares.
pub(crate) fn regression(points: &VecDeque<(f64, f64)>) -> Option<Fit> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let (mut st, mut sy) = (0., 0.);
    for &(t, y) in points {
        st += t;
        sy += y;
    }
    let (mt, my) = (st / n, sy / n);
    let (mut xx, mut xy) = (0., 0.);
    for &(t, y) in points {
        let dt = t - mt;
        xx += dt * dt;
        xy += dt * (y - my);
    }
    if xx.abs() < 1e-9 {
        return None;
    }
    let b = xy / xx;
    let a = my - b * mt;
    let mut sq = 0.;
    for &(t, y) in points {
        let r = y - (a + b * t);
        sq += r * r;
    }
    Some(Fit {
        a,
        b,
        residual: (sq / n).sqrt(),
    })
}
#[derive(Clone, Debug)]
pub struct ClockConfig {
    pub max_age_seconds: f64,
    pub stale_after_ns: u64,
    pub max_points: usize,
    pub min_points_for_fit: usize,
    pub min_points_usable: usize,
    pub min_span_seconds: f64,
    pub max_residual_frames: f64,
    pub jump_reset_frames: f64,
}
impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            max_age_seconds: 4.,
            stale_after_ns: 4_000_000_000,
            max_points: 120,
            min_points_for_fit: 2,
            min_points_usable: 8,
            min_span_seconds: 0.25,
            max_residual_frames: 0.75,
            jump_reset_frames: 5.,
        }
    }
}
#[derive(Default)]
pub struct Clock {
    pub config: ClockConfig,
    points: VecDeque<(f64, f64)>,
    fit: Option<Fit>,
    day_offset: f64,
    last_ns: Option<u64>,
}
#[derive(Clone, Copy, Debug)]
pub struct ClockSnapshot {
    pub a: Option<f64>,
    pub b: Option<f64>,
    pub residual: Option<f64>,
    pub usable: bool,
    pub nominal_fps: Option<u32>,
}
impl Clock {
    pub fn new(config: ClockConfig) -> Self {
        assert!(
            config.min_points_for_fit >= 2
                && config.max_points >= config.min_points_for_fit
                && config.min_points_usable >= config.min_points_for_fit
        );
        Self {
            config,
            ..Self::default()
        }
    }
    pub fn reset(&mut self) {
        self.points.clear();
        self.fit = None;
        self.day_offset = 0.;
        self.last_ns = None;
    }
    pub fn on_signal_lost(&mut self) {
        self.reset();
    }
    pub fn observation_count(&self) -> usize {
        self.points.len()
    }
    pub fn window_span_seconds(&self) -> f64 {
        match (self.points.front(), self.points.back()) {
            (Some(a), Some(b)) => b.0 - a.0,
            _ => 0.,
        }
    }
    pub fn raw_fit(&self) -> Option<Fit> {
        self.fit
    }
    pub fn snapshot(&self, now_ns: u64) -> ClockSnapshot {
        let fresh = self
            .last_ns
            .is_some_and(|last| now_ns.saturating_sub(last) <= self.config.stale_after_ns);
        let fit = self
            .fit
            .filter(|_| fresh && self.points.len() >= self.config.min_points_for_fit);
        let nominal_fps = fit.and_then(|f| {
            let best = [24, 25, 30]
                .into_iter()
                .min_by(|a, b| {
                    (f.b - f64::from(*a))
                        .abs()
                        .total_cmp(&(f.b - f64::from(*b)).abs())
                })
                .unwrap();
            ((f.b - f64::from(best)).abs() <= 0.5).then_some(best)
        });
        ClockSnapshot {
            a: fit.map(|f| f.a),
            b: fit.map(|f| f.b),
            residual: fit.map(|f| f.residual),
            usable: fresh
                && self.points.len() >= self.config.min_points_usable
                && self.window_span_seconds() >= self.config.min_span_seconds
                && self
                    .fit
                    .is_some_and(|f| f.residual <= self.config.max_residual_frames),
            nominal_fps,
        }
    }
    pub fn tc_at_sample_time(&self, t: f64, now_ns: u64) -> Option<f64> {
        self.snapshot(now_ns)
            .usable
            .then(|| self.fit.unwrap().at(t))
    }
    pub fn add_observation(&mut self, t: f64, tc: f64, day: Option<i64>, now_ns: u64) {
        self.last_ns = Some(now_ns);
        let mut candidate = tc + self.day_offset;
        if let Some(f) = self.fit {
            let predicted = f.at(t);
            let diff = candidate - predicted;
            if let Some(day) = day.filter(|d| *d > 0) {
                let d = day as f64;
                if (diff + d).abs() <= self.config.jump_reset_frames {
                    self.day_offset += d;
                    candidate += d;
                } else if (diff - d).abs() <= self.config.jump_reset_frames {
                    self.day_offset -= d;
                    candidate -= d;
                }
            }
            if (candidate - predicted).abs() > self.config.jump_reset_frames {
                self.reset();
                self.last_ns = Some(now_ns);
                candidate = tc;
            }
        }
        self.points.push_back((t, candidate));
        while self.points.len() > self.config.max_points {
            self.points.pop_front();
        }
        while self.points.len() > self.config.min_points_for_fit
            && t - self.points.front().unwrap().0 > self.config.max_age_seconds
        {
            self.points.pop_front();
        }
        if self.points.len() < self.config.min_points_for_fit {
            self.fit = None;
        } else if let Some(f) = regression(&self.points) {
            self.fit = Some(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prune_by_count_and_age_and_identical_times() {
        let mut c = Clock::new(ClockConfig {
            max_points: 10,
            max_age_seconds: 0.1,
            ..Default::default()
        });
        for i in 0..100 {
            c.add_observation(i as f64 / 30., i as f64, None, 0);
        }
        assert!(c.observation_count() <= 4);
        let mut c = Clock::default();
        c.add_observation(1., 1., None, 0);
        c.add_observation(1., 2., None, 0);
        assert!(c.raw_fit().is_none());
    }
}
