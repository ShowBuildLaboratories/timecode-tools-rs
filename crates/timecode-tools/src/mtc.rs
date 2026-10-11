//! MIDI Time Code (MTC) reader for a raw MIDI byte stream.
//!
//! Reads quarter-frame messages (`F1 0nnn dddd`) and full-frame SysEx
//! messages (`F0 7F <device> 01 01 hh mm ss ff F7`). Other MIDI traffic is
//! skipped: channel messages (including running status), other system
//! messages and SysEx, and real-time bytes (`F8`-`FF`) wherever they appear.
//!
//! Eight quarter frames, sent over two frames, carry the frame that began at
//! the first of them (piece 0). Readings come once per frame:
//!
//! - at piece 7, the second frame of the set (`label + 1`), which began at
//!   piece 4;
//! - at piece 3 of the next set, the frame after that (`label + 2`), which
//!   began at its piece 0, provided pieces 0-3 received so far agree with it.
//!
//! Only forward sequences (pieces 0 to 7) are assembled. A reading is
//! `locked` once two complete sets in a row were two frames apart.
//!
//! As elsewhere in this crate, time is passed in by the caller: `feed` takes
//! the monotonic time in nanoseconds at which the bytes arrived.
use crate::label::Label;
use crate::timecode::FrameRate;

fn rate_from_bits(bits: u8) -> FrameRate {
    match bits & 3 {
        0 => FrameRate::Fps24,
        1 => FrameRate::Fps25,
        2 => FrameRate::Fps2997Drop,
        _ => FrameRate::Fps30,
    }
}

fn rate_bits(rate: FrameRate) -> u8 {
    match rate {
        FrameRate::Fps24 => 0,
        FrameRate::Fps25 => 1,
        FrameRate::Fps2997Drop => 2,
        FrameRate::Fps30 => 3,
    }
}

/// How a reading was carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MtcKind {
    /// Eight quarter frames (running transport).
    QuarterFrame,
    /// Full-frame SysEx (usually a locate while stopped).
    FullFrame,
}

/// One MTC reading.
#[derive(Clone, Copy, Debug)]
pub struct MtcFrame {
    pub label: Label,
    pub rate: FrameRate,
    pub kind: MtcKind,
    /// Monotonic time (ns) at which `label` began.
    pub began_ns: u64,
    /// Two complete quarter-frame sets in a row were two frames apart, so
    /// playback is running. Always false for full frames.
    pub locked: bool,
    /// This reading is the frame right after the previous reading.
    pub follows: bool,
}

/// Complete, in-order quarter-frame sets needed before readings are locked.
pub const LOCK_SETS: u32 = 2;

const MAX_SYSEX: usize = 32;

/// Streaming MTC parser.
#[derive(Default)]
pub struct MtcDecoder {
    status: Option<u8>,
    data: [u8; 2],
    data_len: usize,
    sysex: Option<Vec<u8>>,
    pieces: [u8; 8],
    next_piece: u8,
    piece0_ns: u64,
    piece4_ns: u64,
    // Frame index and rate carried by the last complete set.
    last_set: Option<(i64, FrameRate)>,
    contiguous_sets: u32,
    // Index of the last reading, for `follows`.
    last_reading: Option<i64>,
}

impl MtcDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets partial messages and continuity.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Parses MIDI bytes that arrived at `now_ns` (monotonic nanoseconds).
    pub fn feed(&mut self, bytes: &[u8], now_ns: u64) -> Vec<MtcFrame> {
        let mut out = Vec::new();
        for &b in bytes {
            self.byte(b, now_ns, &mut out);
        }
        out
    }

    fn byte(&mut self, b: u8, now_ns: u64, out: &mut Vec<MtcFrame>) {
        if b >= 0xF8 {
            return; // real-time: may appear anywhere, even inside SysEx
        }
        if let Some(buf) = self.sysex.as_mut() {
            if b == 0xF7 {
                let buf = self.sysex.take().unwrap();
                self.full_frame(&buf, now_ns, out);
                return;
            }
            if b < 0x80 {
                if buf.len() < MAX_SYSEX {
                    buf.push(b);
                }
                return;
            }
            self.sysex = None; // a status byte ends an unterminated SysEx
        }
        if b >= 0x80 {
            self.data_len = 0;
            self.status = match b {
                0xF0 => {
                    self.sysex = Some(Vec::new());
                    None
                }
                // Undefined, tune request and a stray end of SysEx carry no data.
                0xF4..=0xF7 => None,
                _ => Some(b),
            };
            return;
        }
        let Some(status) = self.status else {
            return; // stray data byte
        };
        let needed = match status {
            0xF1 | 0xF3 => 1,
            0xF2 => 2,
            0xC0..=0xDF => 1,
            0x80..=0xEF => 2,
            _ => {
                self.status = None;
                return;
            }
        };
        self.data[self.data_len] = b;
        self.data_len += 1;
        if self.data_len < needed {
            return;
        }
        self.data_len = 0;
        if status >= 0xF0 {
            self.status = None; // no running status for system messages
        }
        if status == 0xF1 {
            self.quarter_frame(self.data[0], now_ns, out);
        }
    }

    fn quarter_frame(&mut self, data: u8, now_ns: u64, out: &mut Vec<MtcFrame>) {
        let piece = (data >> 4) & 7;
        if piece == 0 {
            self.next_piece = 0;
            self.piece0_ns = now_ns;
        }
        if piece != self.next_piece {
            // Out of order (dropped bytes or reverse play): start over.
            self.next_piece = 0;
            self.contiguous_sets = 0;
            self.last_set = None;
            self.last_reading = None;
            return;
        }
        self.pieces[piece as usize] = data & 0x0F;
        self.next_piece = (piece + 1) % 8;
        if piece == 4 {
            self.piece4_ns = now_ns;
        }
        if piece == 3 {
            // The frame after the last set's second frame began at this
            // set's piece 0. Report it if the pieces so far agree.
            if let Some((last, rate)) = self.last_set {
                let label = rate.label_at(last + 2);
                let p = self.pieces;
                if p[0] == label.frames & 0xF
                    && p[1] & 1 == label.frames >> 4
                    && p[2] == label.seconds & 0xF
                    && p[3] & 3 == label.seconds >> 4
                {
                    let began = self.piece0_ns;
                    self.reading(label, rate, began, out);
                }
            }
            return;
        }
        if piece != 7 {
            return;
        }
        let p = self.pieces;
        let rate = rate_from_bits(p[7] >> 1);
        let label = Label {
            frames: p[0] | (p[1] & 1) << 4,
            seconds: p[2] | (p[3] & 3) << 4,
            minutes: p[4] | (p[5] & 3) << 4,
            hours: p[6] | (p[7] & 1) << 4,
            drop_frame: rate.drop_frame(),
        };
        let Some(index) = rate.index_of(label) else {
            self.contiguous_sets = 0;
            self.last_set = None;
            self.last_reading = None;
            return;
        };
        let day = rate.frames_per_day();
        self.contiguous_sets = match self.last_set {
            Some((last, r)) if r == rate && (last + 2) % day == index => self.contiguous_sets + 1,
            _ => 1,
        };
        self.last_set = Some((index, rate));
        let began = self.piece4_ns;
        self.reading(rate.label_at(index + 1), rate, began, out);
    }

    fn reading(&mut self, label: Label, rate: FrameRate, began_ns: u64, out: &mut Vec<MtcFrame>) {
        let index = label.frame_index(rate.nominal_fps());
        let follows = self
            .last_reading
            .is_some_and(|last| (last + 1) % rate.frames_per_day() == index);
        self.last_reading = Some(index);
        out.push(MtcFrame {
            label,
            rate,
            kind: MtcKind::QuarterFrame,
            began_ns,
            locked: self.contiguous_sets >= LOCK_SETS,
            follows,
        });
    }

    fn full_frame(&mut self, buf: &[u8], now_ns: u64, out: &mut Vec<MtcFrame>) {
        // 7F <device> 01 01 hh mm ss ff
        if buf.len() != 8 || buf[0] != 0x7F || buf[2] != 0x01 || buf[3] != 0x01 {
            return;
        }
        let rate = rate_from_bits(buf[4] >> 5);
        let label = Label {
            hours: buf[4] & 0x1F,
            minutes: buf[5],
            seconds: buf[6],
            frames: buf[7],
            drop_frame: rate.drop_frame(),
        };
        if rate.index_of(label).is_none() {
            return;
        }
        // A locate: quarter-frame counting starts over.
        self.next_piece = 0;
        self.contiguous_sets = 0;
        self.last_set = None;
        self.last_reading = None;
        out.push(MtcFrame {
            label,
            rate,
            kind: MtcKind::FullFrame,
            began_ns: now_ns,
            locked: false,
            follows: false,
        });
    }
}

/// Encodes MTC for tests: quarter frames for `frame_count` frames starting at
/// `start` (frame index), two frames per eight-piece sequence. Returns the
/// bytes of each quarter-frame message with its send time in nanoseconds.
pub fn quarter_frames(rate: FrameRate, start: i64, frame_count: usize) -> Vec<([u8; 2], u64)> {
    let period_ns = 1e9 / rate.fps();
    let mut out = Vec::with_capacity(frame_count * 4);
    let mut frame = 0;
    while frame + 2 <= frame_count {
        let l = Label::from_index(start + frame as i64, rate.nominal_fps(), rate.drop_frame());
        let bits = rate_bits(rate);
        let nibbles = [
            l.frames & 0xF,
            l.frames >> 4,
            l.seconds & 0xF,
            l.seconds >> 4,
            l.minutes & 0xF,
            l.minutes >> 4,
            l.hours & 0xF,
            (l.hours >> 4) | bits << 1,
        ];
        for (piece, nibble) in nibbles.into_iter().enumerate() {
            let at = ((frame as f64 + piece as f64 / 4.) * period_ns).round() as u64;
            out.push(([0xF1, (piece as u8) << 4 | nibble], at));
        }
        frame += 2;
    }
    out
}

/// Encodes a full-frame SysEx message (device ID 0x7F, "all devices").
pub fn full_frame(rate: FrameRate, label: Label) -> [u8; 10] {
    let bits = rate_bits(rate);
    [
        0xF0,
        0x7F,
        0x7F,
        0x01,
        0x01,
        bits << 5 | label.hours,
        label.minutes,
        label.seconds,
        label.frames,
        0xF7,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rate: FrameRate, start: i64, frames: usize) -> Vec<MtcFrame> {
        let mut d = MtcDecoder::new();
        quarter_frames(rate, start, frames)
            .into_iter()
            .flat_map(|(msg, at)| d.feed(&msg, at))
            .collect()
    }

    fn labels(out: &[MtcFrame]) -> Vec<String> {
        out.iter().map(|f| f.label.to_string()).collect()
    }

    #[test]
    fn one_reading_per_frame_at_every_rate() {
        for rate in FrameRate::ALL {
            let start = rate
                .index_of(Label {
                    hours: 1,
                    minutes: 0,
                    seconds: 59,
                    frames: 0,
                    drop_frame: false,
                })
                .unwrap();
            let out = run(rate, start, 100);
            // Set k gives start+2k+1 at piece 7; from set 1 on, piece 3 also
            // gives start+2k.
            assert_eq!(out.len(), 99);
            for (i, f) in out.iter().enumerate() {
                assert_eq!(f.rate, rate);
                assert_eq!(
                    f.label.frame_index(rate.nominal_fps()),
                    start + i as i64 + 1
                );
                assert_eq!(f.follows, i > 0);
                // Locked from the second complete set (reading 2) on.
                assert_eq!(f.locked, i >= 2, "{i}");
                let began = (i + 1) as f64 * 1e9 / rate.fps();
                assert!((f.began_ns as f64 - began).abs() <= 1.);
            }
        }
        let out = run(FrameRate::Fps2997Drop, 1798, 6);
        assert_eq!(
            labels(&out),
            [
                "00:00:59;29",
                "00:01:00;02",
                "00:01:00;03",
                "00:01:00;04",
                "00:01:00;05"
            ]
        );
    }

    #[test]
    fn skips_other_midi_traffic_and_running_status() {
        let mut d = MtcDecoder::new();
        let mut out = Vec::new();
        for (i, (msg, at)) in quarter_frames(FrameRate::Fps25, 90_000, 8)
            .into_iter()
            .enumerate()
        {
            // Note-on with running status, clock bytes and a foreign SysEx.
            out.extend(d.feed(&[0x90, 60, 100, 61, 0], at));
            out.extend(d.feed(&[0xF8, msg[0], 0xFE, msg[1]], at));
            if i == 3 {
                out.extend(d.feed(&[0xF0, 0x43, 0x10, 0xF8, 0x01, 0xF7], at));
            }
        }
        assert_eq!(
            labels(&out),
            [
                "01:00:00:01",
                "01:00:00:02",
                "01:00:00:03",
                "01:00:00:04",
                "01:00:00:05",
                "01:00:00:06",
                "01:00:00:07"
            ]
        );
    }

    #[test]
    fn relocate_is_not_filled_in() {
        let mut d = MtcDecoder::new();
        let mut out = Vec::new();
        for (msg, at) in quarter_frames(FrameRate::Fps25, 0, 4) {
            out.extend(d.feed(&msg, at));
        }
        for (msg, at) in quarter_frames(FrameRate::Fps25, 500, 4) {
            out.extend(d.feed(&msg, at + 160_000_000));
        }
        // No predicted 00:00:00:04 after the relocate; the new set reports
        // its own second frame.
        assert_eq!(
            labels(&out),
            [
                "00:00:00:01",
                "00:00:00:02",
                "00:00:00:03",
                "00:00:20:01",
                "00:00:20:02",
                "00:00:20:03"
            ]
        );
        assert!(!out[3].follows && out[4].follows);
    }

    #[test]
    fn full_frame_and_broken_sequences() {
        let label = Label {
            hours: 10,
            minutes: 20,
            seconds: 30,
            frames: 29,
            drop_frame: true,
        };
        let mut d = MtcDecoder::new();
        let msg = full_frame(FrameRate::Fps2997Drop, label);
        assert!(d.feed(&msg[..4], 5).is_empty());
        let out = d.feed(&msg[4..], 7);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, label);
        assert_eq!(out[0].kind, MtcKind::FullFrame);
        assert_eq!(out[0].began_ns, 7);
        // Invalid: frame 30 at 30 fps, a skipped drop-frame label, wrong sub-ID.
        assert!(d
            .feed(
                &full_frame(
                    FrameRate::Fps30,
                    Label {
                        frames: 30,
                        ..label
                    }
                ),
                0
            )
            .is_empty());
        assert!(d
            .feed(
                &full_frame(
                    FrameRate::Fps2997Drop,
                    Label {
                        minutes: 1,
                        seconds: 0,
                        frames: 0,
                        ..label
                    }
                ),
                0
            )
            .is_empty());
        let mut bad = msg;
        bad[3] = 0x02;
        assert!(d.feed(&bad, 0).is_empty());
        // A missing piece drops that set and the fill-in after it.
        let qf: Vec<_> = quarter_frames(FrameRate::Fps25, 0, 6)
            .into_iter()
            .map(|(m, _)| m)
            .collect();
        let mut d = MtcDecoder::new();
        let mut out = Vec::new();
        for (i, m) in qf.iter().enumerate() {
            if i != 10 {
                out.extend(d.feed(m, 0));
            }
        }
        assert_eq!(labels(&out), ["00:00:00:01", "00:00:00:05"]);
        assert!(!out[1].follows && !out[1].locked);
    }
}
