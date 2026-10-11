//! LTC from interleaved PCM: channel selection, frame-rate resolution,
//! offset and timestamps on top of [`crate::decoder::Decoder`].
use crate::{
    decoder::{pcm16, Decoder},
    hostmap::{HostMap, IoStamp},
    session::{Channel, Session, SessionError},
    timecode::{offset_label, FrameRate, Timecode},
};
use std::fmt;

/// Interleaved samples: 16-bit integers, or floats in -1..1.
#[derive(Clone, Copy, Debug)]
pub enum Samples<'a> {
    I16(&'a [i16]),
    F32(&'a [f32]),
}

impl Samples<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::I16(s) => s.len(),
            Self::F32(s) => s.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One block of audio.
#[derive(Clone, Copy, Debug)]
pub struct PcmChunk<'a> {
    pub samples: Samples<'a>,
    pub sample_rate: u32,
    /// Interleaved channels (at least 1).
    pub channels: u16,
    /// Monotonic time (ns) at which the first sample was captured.
    pub timestamp_ns: Option<u64>,
}

/// Invalid input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LtcError {
    SampleRate(u32),
    Channels(u16),
    Channel { channel: u8, channels: u16 },
    BlockLength { len: usize, channels: u16 },
}

impl fmt::Display for LtcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SampleRate(r) => write!(f, "sample rate must be 8000..=768000 Hz, got {r}"),
            Self::Channels(c) => write!(f, "channels must be 1..=64, got {c}"),
            Self::Channel { channel, channels } => {
                write!(
                    f,
                    "channel {channel} is out of range for {channels} channel(s)"
                )
            }
            Self::BlockLength { len, channels } => {
                write!(
                    f,
                    "block length {len} is not a multiple of {channels} channels"
                )
            }
        }
    }
}

impl std::error::Error for LtcError {}

/// Options for [`LtcDecoder`] and the receivers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// Fixed frame rate, or `None` to detect it from the signal.
    pub frame_rate: Option<FrameRate>,
    /// Added to every label (latency compensation).
    pub offset_frames: i64,
}

/// LTC decoder for interleaved PCM. Push consecutive blocks; each call
/// returns the frames completed in that block, locked or not.
pub struct LtcDecoder {
    session: Session,
    hostmap: HostMap,
    sample_rate: u32,
    channels: u16,
    next_sample: u64,
    last_stamp_ns: Option<u64>,
    last_drop: bool,
    pub options: ReadOptions,
}

pub(crate) fn check_format(sample_rate: u32, channels: u16) -> Result<(), LtcError> {
    if !(8_000..=768_000).contains(&sample_rate) {
        return Err(LtcError::SampleRate(sample_rate));
    }
    if !(1..=64).contains(&channels) {
        return Err(LtcError::Channels(channels));
    }
    Ok(())
}

fn channel_for(channel: Option<u8>, channels: u16) -> Result<Channel, LtcError> {
    match channel {
        // With one channel there is nothing to elect; decoding it directly
        // also returns the frames before the first lock.
        None | Some(0) if channels == 1 => Ok(Channel::Fixed(1)),
        None | Some(0) => Ok(Channel::Auto),
        Some(n) if u16::from(n) <= channels && n <= 32 => Ok(Channel::Fixed(n)),
        Some(n) => Err(LtcError::Channel {
            channel: n,
            channels,
        }),
    }
}

impl LtcDecoder {
    /// `channel`: one-based channel to decode, or `None` to pick the first
    /// channel that locks.
    pub fn new(
        sample_rate: u32,
        channels: u16,
        channel: Option<u8>,
        options: ReadOptions,
    ) -> Result<Self, LtcError> {
        check_format(sample_rate, channels)?;
        Ok(Self {
            session: Session::new(
                sample_rate,
                channels.into(),
                channel_for(channel, channels)?,
            ),
            hostmap: HostMap::new(sample_rate),
            sample_rate,
            channels,
            next_sample: 0,
            last_stamp_ns: None,
            last_drop: false,
            options,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Selects a one-based channel, or automatic selection with `None`.
    pub fn select_channel(&mut self, channel: Option<u8>) -> Result<(), LtcError> {
        let channel = channel_for(channel, self.channels)?;
        self.session.select(channel);
        Ok(())
    }

    /// One-based channel being decoded, or `None` while scanning.
    pub fn active_channel(&self) -> Option<u32> {
        self.session.active_channel().map(|c| c as u32)
    }

    fn active(&self) -> Option<&Decoder> {
        self.session
            .active_channel()
            .and_then(|c| self.session.decoders().get(c - 1))
    }

    pub fn locked(&self) -> bool {
        self.active().is_some_and(|d| d.locked())
    }

    /// Configured rate, or the detected one.
    pub fn frame_rate(&self) -> Option<FrameRate> {
        self.options.frame_rate.or_else(|| {
            self.active()
                .and_then(|d| FrameRate::from_decoded(d.estimated_fps(), self.last_drop))
        })
    }

    /// Peak level 0..1 of the active channel, or the loudest while scanning.
    pub fn signal_level(&self) -> f64 {
        match self.active() {
            Some(d) => d.signal_level(),
            None => self
                .session
                .decoders()
                .iter()
                .map(|d| d.signal_level())
                .fold(0., f64::max),
        }
    }

    /// Samples per channel decoded so far.
    pub fn samples_decoded(&self) -> u64 {
        self.next_sample
    }

    /// Measured drift of the audio clock against the timestamp clock (ppm).
    pub fn device_ppm(&self) -> Option<f64> {
        self.hostmap.device_ppm()
    }

    /// Decodes one block. `timestamp_ns` is the monotonic capture time of
    /// its first sample; it gives each frame a `timestamp_ns`.
    pub fn push(
        &mut self,
        samples: Samples<'_>,
        timestamp_ns: Option<u64>,
    ) -> Result<Vec<Timecode>, LtcError> {
        let channels = usize::from(self.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(LtcError::BlockLength {
                len: samples.len(),
                channels: self.channels,
            });
        }
        let converted;
        let pcm: &[i16] = match samples {
            Samples::I16(s) => s,
            Samples::F32(s) => {
                converted = s.iter().map(|&x| pcm16(x)).collect::<Vec<_>>();
                &converted
            }
        };
        let first = self.next_sample;
        let count = (pcm.len() / channels) as u64;
        if let Some(ns) = timestamp_ns {
            // A timestamp that does not advance restarts the mapping.
            let discontinuity = self.last_stamp_ns.is_some_and(|last| ns <= last);
            self.hostmap.add_stamp(IoStamp {
                first_sample: first,
                host_ns: ns,
                frames: count as u32,
                discontinuity,
            });
            self.last_stamp_ns = Some(ns);
        }
        // Channel re-election uses time derived from the sample position, so
        // decoding does not depend on how fast the audio is delivered.
        let now_ns = (u128::from(first) * 1_000_000_000 / u128::from(self.sample_rate)) as u64;
        let events = self.session.feed(first, pcm, now_ns).map_err(|e| match e {
            SessionError::InvalidBlock => LtcError::BlockLength {
                len: pcm.len(),
                channels: self.channels,
            },
            SessionError::ChannelUnavailable { selected, .. } => LtcError::Channel {
                channel: selected,
                channels: self.channels,
            },
        })?;
        self.next_sample += count;
        let channel = self.session.active_channel().map(|c| c as u32);
        // Until the regression has two blocks, map linearly from this one.
        let mapping = timestamp_ns.map(|ns| {
            self.hostmap.mapping().unwrap_or((
                first as f64,
                ns as f64,
                1e9 / f64::from(self.sample_rate),
            ))
        });
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            let label = e.frame.label;
            self.last_drop = label.drop_frame;
            let frame_rate = self
                .options
                .frame_rate
                .or_else(|| FrameRate::from_decoded(e.estimated_fps, label.drop_frame));
            let (label, timestamp_ns) = match frame_rate {
                Some(rate) => {
                    // The frame's final edge is where the next frame begins;
                    // this timecode began one frame period earlier.
                    let ts = mapping.map(|(s0, h0, k)| {
                        let end = h0 + k * (e.frame.sample_index as f64 - s0);
                        (end - 1e9 / rate.fps()).max(0.).round() as u64
                    });
                    (offset_label(label, rate, self.options.offset_frames), ts)
                }
                None => (label, None),
            };
            out.push(Timecode {
                label,
                frame_rate,
                locked: e.locked,
                timestamp_ns,
                sample_index: Some(e.frame.sample_index),
                channel,
            });
        }
        Ok(out)
    }
}
