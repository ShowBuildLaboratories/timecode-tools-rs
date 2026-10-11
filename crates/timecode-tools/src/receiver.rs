//! Receivers: follow a timecode source and report frames, lock, loss and
//! jumps. [`LtcReceiver`] reads PCM chunks and [`MtcReceiver`] reads MIDI
//! bytes; both implement [`TimecodeReceiver`].
//!
//! The receivers are synchronous and runtime-free: each `push` returns the
//! events it caused. Delivering them as callbacks, streams or futures is up
//! to the caller (the Dart and Node.js packages do this on top of the same
//! logic).
//!
//! ```
//! use timecode_tools::receiver::{ReceiverEvent, TimecodeReceiver};
//!
//! // Works the same for LTC and MTC.
//! fn follow<R: TimecodeReceiver>(rx: &mut R, chunk: R::Chunk<'_>) -> Result<(), R::Error> {
//!     for event in rx.push(chunk)? {
//!         match event {
//!             ReceiverEvent::Lock(tc) => println!("lock {} {:?}", tc.label, tc.frame_rate),
//!             ReceiverEvent::Frame { timecode, .. } => println!("{}", timecode.label),
//!             ReceiverEvent::Jump(tc) => println!("jump {}", tc.label),
//!             ReceiverEvent::Lost => println!("lost"),
//!         }
//!     }
//!     Ok(())
//! }
//! # use timecode_tools::{ltc::{PcmChunk, Samples}, receiver::LtcReceiver};
//! # let pcm = timecode_tools::synth::generate(Default::default()).pcm;
//! # let mut rx = LtcReceiver::default();
//! # follow(&mut rx, PcmChunk { samples: Samples::I16(&pcm), sample_rate: 48_000, channels: 1, timestamp_ns: Some(0) }).unwrap();
//! # assert!(rx.locked());
//! ```
use crate::{
    clock::Clock,
    label::Label,
    ltc::{check_format, LtcDecoder, LtcError, PcmChunk, ReadOptions},
    mtc::{MtcDecoder, MtcKind},
    timecode::{offset_label, FrameRate, Timecode},
};
use std::{convert::Infallible, time::Duration};

/// Options shared by both receivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverOptions {
    /// Fixed frame rate, or `None` to take it from the source.
    pub frame_rate: Option<FrameRate>,
    /// Added to every reported timecode (latency compensation). Positive
    /// when the signal reaches you late.
    pub offset_frames: i64,
    /// How long without a followed frame before [`ReceiverEvent::Lost`].
    pub lost_after: Duration,
    /// LTC only: one-based channel to read from multichannel chunks, or
    /// `None` to scan all channels and follow the first that locks.
    pub channel: Option<u8>,
}

impl Default for ReceiverOptions {
    fn default() -> Self {
        Self {
            frame_rate: None,
            offset_frames: 0,
            lost_after: Duration::from_millis(300),
            channel: Some(1),
        }
    }
}

/// What a receiver reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReceiverEvent {
    /// The source became stable. Followed by the first `Frame`.
    Lock(Timecode),
    /// A timecode on the followed timeline (only while locked).
    Frame {
        timecode: Timecode,
        /// Frame index of the reading just before this one, when this one
        /// continues it (normal playback); `None` after a jump.
        previous: Option<i64>,
    },
    /// While locked, the timecode moved discontinuously (confirmed by two
    /// contiguous readings); or an MTC full-frame message set a position.
    Jump(Timecode),
    /// Nothing followed for `lost_after`, the input format changed, an MTC
    /// locate stopped the chase, or `signal_lost` was called.
    Lost,
}

impl ReceiverEvent {
    /// Whether this event passes `target` during normal playback: a `Frame`
    /// that continues the previous reading and is at or past `target` while
    /// the previous one was before it. A jump over (or onto) the target does
    /// not count. Handles midnight.
    pub fn passes(&self, target: Label) -> bool {
        let Self::Frame {
            timecode,
            previous: Some(previous),
        } = self
        else {
            return false;
        };
        let (Some(rate), Some(index)) = (timecode.frame_rate, timecode.frame_index()) else {
            return false;
        };
        let Some(target) = rate.index_of(target) else {
            return false;
        };
        let day = rate.frames_per_day();
        let step = (index - previous).rem_euclid(day);
        step > 0 && (index - target).rem_euclid(day) < step
    }
}

/// The interface shared by [`LtcReceiver`] and [`MtcReceiver`].
pub trait TimecodeReceiver {
    /// Input: [`PcmChunk`] for LTC, [`MidiChunk`] for MTC.
    type Chunk<'a>;
    type Error;
    /// Processes one chunk and returns the events it caused, in order.
    fn push(&mut self, chunk: Self::Chunk<'_>) -> Result<Vec<ReceiverEvent>, Self::Error>;
    /// Ends the chase now (input stopped, detached or closed). Returns
    /// `[Lost]` if it was locked.
    fn signal_lost(&mut self) -> Vec<ReceiverEvent>;
    fn locked(&self) -> bool;
    /// The most recent followed timecode (or MTC locate position).
    fn current(&self) -> Option<Timecode>;
    /// The configured rate (`None`: from the source).
    fn frame_rate(&self) -> Option<FrameRate>;
    /// The rate in use: configured, or detected; `None` until known.
    fn detected_rate(&self) -> Option<FrameRate>;
    fn offset_frames(&self) -> i64;
    fn set_offset_frames(&mut self, frames: i64);
    /// The position at `timestamp_ns` (the input timestamp clock) as a
    /// fractional frame count since 00:00:00:00 (not wrapped at midnight),
    /// offset applied, from a least-squares fit of the recent signal. `None`
    /// until locked long enough to fit, or after loss.
    fn frames_at(&self, timestamp_ns: u64) -> Option<f64>;
}

/// Lock / jump / loss state shared by both receivers. `clock` values are in
/// seconds on the receiver's own time base.
struct Tracker {
    lost_after: f64,
    locked: bool,
    current: Option<Timecode>,
    rate: Option<FrameRate>,
    // The last reading of any kind, and the last one followed while locked.
    read: Option<(i64, f64)>,
    followed: (i64, f64),
    pending: Option<(i64, f64)>,
    // Regression of timecode against source time, and an exponentially
    // smoothed offset from source time to the input timestamp clock (us).
    clock: Clock,
    host_offset_us: Option<f64>,
}

impl Tracker {
    fn new(lost_after: Duration) -> Self {
        Self {
            lost_after: lost_after.as_secs_f64(),
            locked: false,
            current: None,
            rate: None,
            read: None,
            followed: (0, 0.),
            pending: None,
            clock: Clock::default(),
            host_offset_us: None,
        }
    }

    /// The input clock read `host_ns` at `source_time` seconds.
    fn note_host(&mut self, source_time: f64, host_ns: u64) {
        let offset = host_ns as f64 / 1e3 - source_time * 1e6;
        self.host_offset_us = Some(match self.host_offset_us {
            None => offset,
            Some(previous) => previous + 0.2 * (offset - previous),
        });
    }

    /// One clock observation: `frames` (before the offset) at `source_time`.
    fn observe(&mut self, source_time: f64, frames: f64, day: i64, now_ns: u64) {
        self.clock
            .add_observation(source_time, frames, Some(day), now_ns);
    }

    fn reset_clock(&mut self) {
        self.clock.reset();
        self.host_offset_us = None;
    }

    fn frames_at(&self, timestamp_ns: u64, offset_frames: i64) -> Option<f64> {
        if !self.locked {
            return None;
        }
        self.rate?;
        let offset = self.host_offset_us?;
        // Keep sub-microsecond precision: callers may ask at any instant.
        let source_time = (timestamp_ns as f64 / 1e3 - offset) / 1e6;
        self.clock
            .tc_at_sample_time(source_time, timestamp_ns)
            .map(|f| f + offset_frames as f64)
    }

    // `follows`: the source itself says this reading is the frame right after
    // the previous reading (MTC). Otherwise the expected step comes from
    // elapsed time, so a frame lost to a dropout or replaced by a corrupt one
    // does not break continuity.
    fn on_reading(
        &mut self,
        tc: Timecode,
        clock: f64,
        follows: bool,
        out: &mut Vec<ReceiverEvent>,
    ) {
        let (Some(rate), Some(index)) = (tc.frame_rate, tc.frame_index()) else {
            return;
        };
        self.rate = Some(rate);
        let day = rate.frames_per_day();
        let next = |from: (i64, f64)| {
            let step = if follows {
                1
            } else {
                (((clock - from.1) * rate.fps()).round() as i64).max(1)
            };
            (from.0 + step).rem_euclid(day)
        };
        let continues_read = self.read.filter(|&r| next(r) == index).map(|r| r.0);
        self.read = Some((index, clock));
        if !self.locked {
            if !tc.locked {
                return;
            }
            self.locked = true;
            self.pending = None;
            self.follow(tc, index, clock);
            out.push(ReceiverEvent::Lock(tc));
            out.push(ReceiverEvent::Frame {
                timecode: tc,
                previous: continues_read,
            });
            return;
        }
        if index == next(self.followed) {
            let previous = self.followed.0;
            self.pending = None;
            self.follow(tc, index, clock);
            out.push(ReceiverEvent::Frame {
                timecode: tc,
                previous: Some(previous),
            });
        } else if self.pending.is_some_and(|p| index == next(p)) {
            // Two contiguous readings on a new timeline confirm a jump; a
            // single stray one is ignored.
            self.pending = None;
            self.follow(tc, index, clock);
            out.push(ReceiverEvent::Jump(tc));
            out.push(ReceiverEvent::Frame {
                timecode: tc,
                previous: None,
            });
        } else {
            self.pending = Some((index, clock));
        }
    }

    fn follow(&mut self, tc: Timecode, index: i64, clock: f64) {
        self.current = Some(tc);
        self.followed = (index, clock);
    }

    // A position given outright (MTC full frame): ends any chase and reports
    // the new position as a jump.
    fn locate(&mut self, tc: Timecode, out: &mut Vec<ReceiverEvent>) {
        self.lost(out);
        self.current = Some(tc);
        self.rate = tc.frame_rate.or(self.rate);
        self.read = None;
        out.push(ReceiverEvent::Jump(tc));
    }

    fn check_loss(&mut self, now: f64, out: &mut Vec<ReceiverEvent>) {
        if self.locked && now - self.followed.1 > self.lost_after {
            self.lost(out);
        }
    }

    fn lost(&mut self, out: &mut Vec<ReceiverEvent>) {
        self.pending = None;
        self.read = None;
        if self.locked {
            self.locked = false;
            self.reset_clock();
            out.push(ReceiverEvent::Lost);
        }
    }
}

/// Follows LTC in PCM chunks. A change of sample rate or channel count
/// restarts decoding.
pub struct LtcReceiver {
    options: ReceiverOptions,
    tracker: Tracker,
    decoder: Option<LtcDecoder>,
}

impl Default for LtcReceiver {
    fn default() -> Self {
        Self::new(ReceiverOptions::default())
    }
}

impl LtcReceiver {
    pub fn new(options: ReceiverOptions) -> Self {
        Self {
            options,
            tracker: Tracker::new(options.lost_after),
            decoder: None,
        }
    }

    /// Recent peak input level 0..1.
    pub fn level(&self) -> f64 {
        self.decoder.as_ref().map_or(0., |d| d.signal_level())
    }

    /// One-based channel being read, if known.
    pub fn channel(&self) -> Option<u32> {
        self.decoder.as_ref().and_then(|d| d.active_channel())
    }

    fn read_options(&self) -> ReadOptions {
        ReadOptions {
            frame_rate: self.options.frame_rate,
            offset_frames: self.options.offset_frames,
        }
    }
}

impl TimecodeReceiver for LtcReceiver {
    type Chunk<'a> = PcmChunk<'a>;
    type Error = LtcError;

    fn push(&mut self, chunk: PcmChunk<'_>) -> Result<Vec<ReceiverEvent>, LtcError> {
        check_format(chunk.sample_rate, chunk.channels)?;
        let mut out = Vec::new();
        let same = self.decoder.as_ref().is_some_and(|d| {
            d.sample_rate() == chunk.sample_rate && d.channels() == chunk.channels
        });
        if !same {
            self.tracker.lost(&mut out);
            self.decoder = Some(LtcDecoder::new(
                chunk.sample_rate,
                chunk.channels,
                self.options.channel,
                self.read_options(),
            )?);
            self.tracker.reset_clock();
        }
        let decoder = self.decoder.as_mut().unwrap();
        let rate = f64::from(chunk.sample_rate);
        if let Some(ns) = chunk.timestamp_ns {
            self.tracker
                .note_host(decoder.samples_decoded() as f64 / rate, ns);
        }
        let now = chunk.timestamp_ns.unwrap_or(0);
        let offset = self.options.offset_frames;
        // Continuity and loss are judged on sample time, which has no
        // delivery jitter.
        for tc in decoder.push(chunk.samples, chunk.timestamp_ns)? {
            let clock = tc.sample_index.unwrap_or(0) as f64 / rate;
            if let (Some(r), Some(index)) = (tc.frame_rate, tc.frame_index()) {
                // The clock follows the decoded timecode before the offset;
                // the frame completes where the next one begins.
                let day = r.frames_per_day();
                let source = (index - offset).rem_euclid(day);
                self.tracker.observe(clock, (source + 1) as f64, day, now);
            }
            self.tracker.on_reading(tc, clock, false, &mut out);
        }
        let now = decoder.samples_decoded() as f64 / rate;
        self.tracker.check_loss(now, &mut out);
        Ok(out)
    }

    fn signal_lost(&mut self) -> Vec<ReceiverEvent> {
        let mut out = Vec::new();
        self.tracker.lost(&mut out);
        out
    }
    fn locked(&self) -> bool {
        self.tracker.locked
    }
    fn current(&self) -> Option<Timecode> {
        self.tracker.current
    }
    fn frame_rate(&self) -> Option<FrameRate> {
        self.options.frame_rate
    }
    fn detected_rate(&self) -> Option<FrameRate> {
        self.options.frame_rate.or(self.tracker.rate)
    }
    fn offset_frames(&self) -> i64 {
        self.options.offset_frames
    }
    fn set_offset_frames(&mut self, frames: i64) {
        self.options.offset_frames = frames;
        if let Some(d) = self.decoder.as_mut() {
            d.options.offset_frames = frames;
        }
    }
    fn frames_at(&self, timestamp_ns: u64) -> Option<f64> {
        self.tracker
            .frames_at(timestamp_ns, self.options.offset_frames)
    }
}

/// A chunk of raw MIDI bytes and the monotonic time (ns) they arrived.
#[derive(Clone, Copy, Debug)]
pub struct MidiChunk<'a> {
    pub data: &'a [u8],
    pub timestamp_ns: u64,
}

/// Follows MIDI Time Code in MIDI bytes: a reading per frame from quarter
/// frames; a full-frame message (a locate) ends any chase and is reported as
/// a [`ReceiverEvent::Jump`].
pub struct MtcReceiver {
    options: ReceiverOptions,
    tracker: Tracker,
    decoder: MtcDecoder,
}

impl Default for MtcReceiver {
    fn default() -> Self {
        Self::new(ReceiverOptions::default())
    }
}

impl MtcReceiver {
    pub fn new(options: ReceiverOptions) -> Self {
        Self {
            options,
            tracker: Tracker::new(options.lost_after),
            decoder: MtcDecoder::new(),
        }
    }
}

impl TimecodeReceiver for MtcReceiver {
    type Chunk<'a> = MidiChunk<'a>;
    type Error = Infallible;

    fn push(&mut self, chunk: MidiChunk<'_>) -> Result<Vec<ReceiverEvent>, Infallible> {
        let mut out = Vec::new();
        for f in self.decoder.feed(chunk.data, chunk.timestamp_ns) {
            let rate = self.options.frame_rate.unwrap_or(f.rate);
            let tc = Timecode {
                label: offset_label(f.label, rate, self.options.offset_frames),
                frame_rate: Some(rate),
                locked: f.locked,
                timestamp_ns: Some(f.began_ns),
                sample_index: None,
                channel: None,
            };
            match f.kind {
                MtcKind::FullFrame => {
                    self.tracker.reset_clock();
                    self.tracker.locate(tc, &mut out);
                }
                MtcKind::QuarterFrame => {
                    let t = f.began_ns as f64 / 1e9;
                    self.tracker.note_host(t, f.began_ns);
                    let index = f.label.frame_index(f.rate.nominal_fps()) as f64;
                    self.tracker
                        .observe(t, index, f.rate.frames_per_day(), chunk.timestamp_ns);
                    self.tracker.on_reading(tc, t, f.follows, &mut out)
                }
            }
        }
        self.tracker
            .check_loss(chunk.timestamp_ns as f64 / 1e9, &mut out);
        Ok(out)
    }

    fn signal_lost(&mut self) -> Vec<ReceiverEvent> {
        let mut out = Vec::new();
        self.tracker.lost(&mut out);
        out
    }
    fn locked(&self) -> bool {
        self.tracker.locked
    }
    fn current(&self) -> Option<Timecode> {
        self.tracker.current
    }
    fn frame_rate(&self) -> Option<FrameRate> {
        self.options.frame_rate
    }
    fn detected_rate(&self) -> Option<FrameRate> {
        self.options.frame_rate.or(self.tracker.rate)
    }
    fn offset_frames(&self) -> i64 {
        self.options.offset_frames
    }
    fn set_offset_frames(&mut self, frames: i64) {
        self.options.offset_frames = frames;
    }
    fn frames_at(&self, timestamp_ns: u64) -> Option<f64> {
        self.tracker
            .frames_at(timestamp_ns, self.options.offset_frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ltc::Samples,
        mtc::{full_frame, quarter_frames},
        synth::{generate, SynthConfig},
    };

    fn names(events: &[ReceiverEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                ReceiverEvent::Lock(t) => Some(format!("lock {}", t.label)),
                ReceiverEvent::Jump(t) => Some(format!("jump {}", t.label)),
                ReceiverEvent::Lost => Some("lost".into()),
                ReceiverEvent::Frame { .. } => None,
            })
            .collect()
    }

    #[test]
    fn ltc_lock_jump_corrupt_and_loss() {
        let mut pcm = generate(SynthConfig {
            start_index: 90_000,
            frame_count: 40,
            corrupt_frames: vec![(20, 5)],
            ..Default::default()
        })
        .pcm;
        pcm.extend(
            generate(SynthConfig {
                start_index: 180_000,
                frame_count: 40,
                ..Default::default()
            })
            .pcm,
        );
        pcm.extend(vec![0; 48_000]);
        let mut rx = LtcReceiver::default();
        let mut events = Vec::new();
        for (i, block) in pcm.chunks(480).enumerate() {
            events.extend(
                rx.push(PcmChunk {
                    samples: Samples::I16(block),
                    sample_rate: 48_000,
                    channels: 1,
                    timestamp_ns: Some(i as u64 * 10_000_000),
                })
                .unwrap(),
            );
        }
        assert_eq!(
            names(&events),
            ["lock 01:00:00:03", "jump 02:00:00:01", "lost"]
        );
        let target = Label {
            hours: 1,
            minutes: 0,
            seconds: 1,
            frames: 0,
            drop_frame: false,
        };
        assert_eq!(events.iter().filter(|e| e.passes(target)).count(), 1);
        assert_eq!(rx.detected_rate(), Some(FrameRate::Fps25));
        assert_eq!(rx.frame_rate(), None);
    }

    #[test]
    fn mtc_locate_lock_and_offset() {
        let mut rx = MtcReceiver::new(ReceiverOptions {
            offset_frames: 2,
            ..Default::default()
        });
        let start = FrameRate::Fps30.label_at(0);
        let mut events = rx
            .push(MidiChunk {
                data: &full_frame(FrameRate::Fps30, start),
                timestamp_ns: 0,
            })
            .unwrap();
        for (msg, at) in quarter_frames(FrameRate::Fps30, 0, 20) {
            events.extend(
                rx.push(MidiChunk {
                    data: &msg,
                    timestamp_ns: 1_000_000_000 + at,
                })
                .unwrap(),
            );
        }
        events.extend(rx.signal_lost());
        assert_eq!(
            names(&events),
            ["jump 00:00:00:02", "lock 00:00:00:05", "lost"]
        );
        assert_eq!(rx.offset_frames(), 2);
    }

    #[test]
    fn frames_at_keeps_sub_microsecond_precision() {
        let pcm = generate(SynthConfig {
            frame_count: 50,
            ..Default::default()
        })
        .pcm;
        let mut rx = LtcReceiver::default();
        for (i, block) in pcm.chunks(480).enumerate() {
            rx.push(PcmChunk {
                samples: Samples::I16(block),
                sample_rate: 48_000,
                channels: 1,
                timestamp_ns: Some(i as u64 * 10_000_000 + 333),
            })
            .unwrap();
        }
        let t = 1_500_000_333;
        let a = rx.frames_at(t).unwrap();
        let b = rx.frames_at(t + 500).unwrap();
        // 500 ns at 25 fps is 1.25e-5 frames.
        assert!(((b - a) - 1.25e-5).abs() < 1e-9, "{}", b - a);
    }
}
