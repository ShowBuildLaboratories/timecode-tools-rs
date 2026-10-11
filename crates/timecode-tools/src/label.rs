use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}
impl Label {
    pub fn frame_index(self, fps: u32) -> i64 {
        let m = i64::from(self.hours) * 60 + i64::from(self.minutes);
        let raw =
            (i64::from(self.hours) * 3600 + i64::from(self.minutes) * 60 + i64::from(self.seconds))
                * i64::from(fps)
                + i64::from(self.frames);
        raw - if self.drop_frame {
            i64::from(fps * 2 / 30) * (m - m / 10)
        } else {
            0
        }
    }
    pub fn from_index(index: i64, fps: u32, drop_frame: bool) -> Self {
        assert!(fps > 0);
        let f = i64::from(fps);
        let index = index.rem_euclid(frames_per_day(fps, drop_frame));
        let (m, n) = if !drop_frame {
            (index / (60 * f), index % (60 * f))
        } else {
            let d = f * 2 / 30;
            let full = 60 * f;
            let short = full - d;
            let ten = full + 9 * short;
            let block = index / ten;
            let within = index % ten;
            if within < full {
                (block * 10, within)
            } else {
                let after = within - full;
                (block * 10 + 1 + after / short, after % short + d)
            }
        };
        Self {
            hours: (m / 60) as u8,
            minutes: (m % 60) as u8,
            seconds: (n / f) as u8,
            frames: (n % f) as u8,
            drop_frame,
        }
    }
}
impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}{}{:02}",
            self.hours,
            self.minutes,
            self.seconds,
            if self.drop_frame { ';' } else { ':' },
            self.frames
        )
    }
}
pub fn frames_per_day(fps: u32, df: bool) -> i64 {
    Label {
        hours: 23,
        minutes: 59,
        seconds: 59,
        frames: (fps - 1) as u8,
        drop_frame: df,
    }
    .frame_index(fps)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_wrap_and_display() {
        assert_eq!(Label::from_index(-1, 30, true).to_string(), "23:59:59;29");
    }
}
