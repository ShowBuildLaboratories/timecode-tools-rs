//! SMPTE LTC (linear timecode) and MIDI Time Code decoding without hardware
//! or dependencies.
//!
//! - [`receiver`]: [`receiver::LtcReceiver`] and [`receiver::MtcReceiver`]
//!   follow a source and report frames, lock, loss and jumps through the
//!   shared [`receiver::TimecodeReceiver`] trait.
//! - [`ltc`]: LTC from interleaved PCM with channel selection, frame-rate
//!   resolution, offset and timestamps.
//! - [`timecode`]: [`timecode::FrameRate`] and [`timecode::Timecode`].
//! - [`decoder`]: biphase-mark (BMC) decoder for 16-bit PCM. Emits each
//!   frame's label, the sample at which it completed, lock state and the
//!   estimated nominal frame rate (24, 25 or 30).
//! - [`label`]: timecode labels, frame-index arithmetic and drop-frame rules.
//! - [`clock`]: least-squares regression of timecode against sample time,
//!   with staleness, midnight unwrap and jam (discontinuity) reset.
//! - [`hostmap`]: maps device sample positions to host time from IO stamps.
//! - [`model`]: combines the clock and host map into a timecode clock with
//!   lock/freewheel states, jam confirmation and input-latency compensation.
//! - [`session`]: decodes interleaved multichannel PCM, with fixed or
//!   automatic channel selection.
//! - [`mtc`]: MIDI Time Code reader for raw MIDI bytes (quarter-frame and
//!   full-frame messages).
//! - [`synth`]: deterministic LTC test-signal generator for tests, with
//!   optional noise, drift, varispeed, dropouts and relocations.
//!
//! All elapsed times are passed in by the caller as monotonic nanoseconds;
//! nothing reads a system clock, so behavior is reproducible in tests.
//!
//! The decoder, clock and receivers reproduce the pure-Dart reference
//! implementation exactly; the golden files in the repository check this
//! value for value. `model` and
//! `session` add behavior on top: `model` places a decoded frame at the start
//! of the *next* frame (the frame's final edge is where the next one begins),
//! confirms a jam with two contiguous frames and keeps the previous slope
//! while re-anchoring phase; `session` uses caller-supplied monotonic time for
//! its 700 ms channel re-election grace.
//!
//! ```
//! use timecode_tools::{decoder::Decoder, synth::{generate, SynthConfig}};
//!
//! let signal = generate(SynthConfig { frame_count: 10, ..Default::default() });
//! let mut decoder = Decoder::new(48_000);
//! let frames = decoder.feed_at(0, &signal.pcm);
//! assert_eq!(frames.len(), 10);
//! assert_eq!(frames[9].frame.label.to_string(), "00:00:00:09");
//! assert!(frames[9].locked);
//! assert_eq!(frames[9].estimated_fps, Some(25));
//! ```
#![forbid(unsafe_code)]

/// The crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod clock;
pub mod decoder;
pub mod hostmap;
pub mod label;
pub mod ltc;
pub mod model;
pub mod mtc;
pub mod receiver;
pub mod session;
pub mod synth;
pub mod timecode;

// Compile and run the README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
