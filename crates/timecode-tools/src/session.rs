use crate::decoder::{DecodedFrame, Decoder};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Auto,
    Fixed(u8),
}
impl Channel {
    pub fn from_index(index: u8) -> Option<Self> {
        match index {
            1..=32 => Some(Self::Fixed(index)),
            _ => None,
        }
    }
    /// Parses `"auto"` or `"ch1"`..`"ch32"`. Anything else (including
    /// `"ch01"`) is rejected rather than repaired.
    pub fn from_name(name: &str) -> Option<Self> {
        if name == "auto" {
            return Some(Self::Auto);
        }
        let number = name.strip_prefix("ch")?.parse::<u8>().ok()?;
        if name != format!("ch{number}") {
            return None;
        }
        Self::from_index(number)
    }
    pub fn index(self) -> Option<u8> {
        match self {
            Self::Auto => None,
            Self::Fixed(n) => Some(n),
        }
    }
    pub fn label(self) -> String {
        self.index()
            .map_or_else(|| "AUTO".into(), |n| format!("CH {n}"))
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum SessionError {
    ChannelUnavailable { selected: u8, captured: usize },
    InvalidBlock,
}
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChannelUnavailable { selected, captured } => write!(
                f,
                "this device delivered {captured} channels; channel {selected} is unavailable"
            ),
            Self::InvalidBlock => write!(f, "invalid interleaved PCM block"),
        }
    }
}
impl std::error::Error for SessionError {}
/// Decodes interleaved PCM and picks a channel, either fixed or automatically
/// (the first channel to lock). Audio capture itself is up to the caller.
pub struct Session {
    pub channel: Channel,
    decoders: Vec<Decoder>,
    elected: Option<usize>,
    last_locked_ns: Option<u64>,
    pub grace_ns: u64,
    pub duplicate_channels: bool,
    identical_streak: u32,
}
impl Session {
    pub fn new(sample_rate: u32, channels: usize, channel: Channel) -> Self {
        assert!(channels > 0);
        Self {
            channel,
            decoders: (0..channels).map(|_| Decoder::new(sample_rate)).collect(),
            elected: None,
            last_locked_ns: None,
            grace_ns: 700_000_000,
            duplicate_channels: false,
            identical_streak: 0,
        }
    }
    /// Per-channel decoders, in channel order (index 0 is channel 1).
    pub fn decoders(&self) -> &[Decoder] {
        &self.decoders
    }
    /// One-based channel currently decoded, or `None` while AUTO is scanning.
    pub fn active_channel(&self) -> Option<usize> {
        match self.channel {
            Channel::Auto => self.elected.map(|i| i + 1),
            Channel::Fixed(n) => {
                (n > 0 && usize::from(n) <= self.decoders.len()).then_some(usize::from(n))
            }
        }
    }
    pub fn select(&mut self, channel: Channel) {
        self.channel = channel;
        self.elected = None;
        self.last_locked_ns = None;
    }
    pub fn tick(&mut self, now_ns: u64) {
        if self.channel == Channel::Auto
            && self
                .last_locked_ns
                .is_some_and(|last| now_ns.saturating_sub(last) >= self.grace_ns)
        {
            self.elected = None;
            self.last_locked_ns = None;
        }
    }
    pub fn feed(
        &mut self,
        first_sample: u64,
        interleaved: &[i16],
        now_ns: u64,
    ) -> Result<Vec<DecodedFrame>, SessionError> {
        self.tick(now_ns);
        let count = self.decoders.len();
        if !interleaved.len().is_multiple_of(count) {
            return Err(SessionError::InvalidBlock);
        }
        if let Channel::Fixed(n) = self.channel {
            if n == 0 || usize::from(n) > count {
                return Err(SessionError::ChannelUnavailable {
                    selected: n,
                    captured: count,
                });
            }
        }
        let indices: Vec<usize> = self
            .active_channel()
            .map_or_else(|| (0..count).collect(), |n| vec![n - 1]);
        let mut out = Vec::new();
        for i in indices {
            let pcm: Vec<i16> = interleaved.chunks_exact(count).map(|p| p[i]).collect();
            for event in self.decoders[i].feed_at(first_sample, &pcm) {
                if self.channel == Channel::Auto && self.elected.is_none() && event.locked {
                    self.elected = Some(i);
                }
                if self.active_channel() == Some(i + 1) {
                    if event.locked {
                        self.last_locked_ns = Some(now_ns);
                    }
                    out.push(event);
                }
            }
        }
        if count == 2 && !interleaved.is_empty() && !self.duplicate_channels {
            let signal = self.decoders.iter().any(|d| d.signal_level() > 0.02);
            if signal && interleaved.chunks_exact(2).all(|p| p[0] == p[1]) {
                self.identical_streak += 1;
            } else {
                self.identical_streak = 0;
            }
            self.duplicate_channels = self.identical_streak >= 12;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_is_not_duplicate_audio() {
        let mut s = Session::new(48000, 2, Channel::Auto);
        for i in 0..20 {
            s.feed(i * 100, &[0; 200], 0).unwrap();
        }
        assert!(!s.duplicate_channels);
        assert_eq!(s.active_channel(), None);
    }
}
