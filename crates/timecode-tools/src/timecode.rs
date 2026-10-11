//! Frame rates and received timecodes, shared by the LTC and MTC readers.
use crate::label::{frames_per_day, Label};

/// A timecode frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameRate {
    /// 24 fps (23.976 reads the same: the labels are identical).
    Fps24,
    Fps25,
    /// 29.97 fps drop-frame.
    Fps2997Drop,
    /// 30 fps (29.97 non-drop reads the same).
    Fps30,
}

impl FrameRate {
    pub const ALL: [FrameRate; 4] = [Self::Fps24, Self::Fps25, Self::Fps2997Drop, Self::Fps30];

    /// Label rate: 24, 25 or 30.
    pub fn nominal_fps(self) -> u32 {
        match self {
            Self::Fps24 => 24,
            Self::Fps25 => 25,
            Self::Fps2997Drop | Self::Fps30 => 30,
        }
    }

    pub fn drop_frame(self) -> bool {
        self == Self::Fps2997Drop
    }

    /// Real frames per second.
    pub fn fps(self) -> f64 {
        match self {
            Self::Fps2997Drop => 30_000. / 1001.,
            r => f64::from(r.nominal_fps()),
        }
    }

    /// Rate for a decoded nominal fps (if estimated) and drop-frame flag.
    /// The drop-frame flag alone means 29.97 drop-frame.
    pub fn from_decoded(nominal_fps: Option<u32>, drop_frame: bool) -> Option<Self> {
        match (nominal_fps, drop_frame) {
            (Some(30) | None, true) => Some(Self::Fps2997Drop),
            (Some(24), false) => Some(Self::Fps24),
            (Some(25), false) => Some(Self::Fps25),
            (Some(30), false) => Some(Self::Fps30),
            _ => None,
        }
    }

    /// Short name: `"24"`, `"25"`, `"29.97df"` or `"30"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Fps24 => "24",
            Self::Fps25 => "25",
            Self::Fps2997Drop => "29.97df",
            Self::Fps30 => "30",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == name)
    }

    /// Frames in a 24-hour day.
    pub fn frames_per_day(self) -> i64 {
        frames_per_day(self.nominal_fps(), self.drop_frame())
    }

    /// Frame index of `label` if it is a valid timecode at this rate.
    pub fn index_of(self, label: Label) -> Option<i64> {
        let fps = self.nominal_fps();
        let skipped = self.drop_frame()
            && label.seconds == 0
            && !label.minutes.is_multiple_of(10)
            && label.frames < 2;
        (label.hours < 24
            && label.minutes < 60
            && label.seconds < 60
            && u32::from(label.frames) < fps
            && !skipped)
            .then(|| {
                Label {
                    drop_frame: self.drop_frame(),
                    ..label
                }
                .frame_index(fps)
            })
    }

    /// Label for a frame index at this rate, wrapping around one day.
    pub fn label_at(self, index: i64) -> Label {
        Label::from_index(index, self.nominal_fps(), self.drop_frame())
    }
}

/// A received timecode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timecode {
    pub label: Label,
    /// Rate the label is read at; `None` while it is not known yet.
    pub frame_rate: Option<FrameRate>,
    /// Whether the source was stable (locked) when this was received.
    pub locked: bool,
    /// Monotonic time (ns) at which this timecode began, in the clock of
    /// the input timestamps. `None` when no timestamps were given.
    pub timestamp_ns: Option<u64>,
    /// LTC: one-based per-channel sample index of the frame's final edge.
    pub sample_index: Option<u64>,
    /// LTC: one-based channel it was read from.
    pub channel: Option<u32>,
}

impl Timecode {
    /// Frames since 00:00:00:00, if the rate is known.
    pub fn frame_index(&self) -> Option<i64> {
        self.frame_rate
            .map(|r| self.label.frame_index(r.nominal_fps()))
    }
}

/// Moves `label` by `offset` frames at `rate`; the drop-frame flag follows
/// the rate.
pub(crate) fn offset_label(label: Label, rate: FrameRate, offset: i64) -> Label {
    let label = Label {
        drop_frame: rate.drop_frame(),
        ..label
    };
    if offset == 0 {
        return label;
    }
    rate.label_at(label.frame_index(rate.nominal_fps()) + offset)
}
