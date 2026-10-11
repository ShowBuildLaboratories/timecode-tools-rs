# timecode-tools

[![crates.io](https://img.shields.io/crates/v/timecode-tools.svg)](https://crates.io/crates/timecode-tools)
[![docs.rs](https://docs.rs/timecode-tools/badge.svg)](https://docs.rs/timecode-tools)

Timecode readers for Rust: SMPTE LTC (linear timecode) from PCM audio and
MIDI Time Code from MIDI bytes, with timecode arithmetic and a
drift-correcting timecode clock.

- 24, 25, 29.97 drop-frame and 30 fps, detected automatically
- Either signal polarity, any sample rate, any block size
- Multichannel input with fixed or automatic channel selection
- No dependencies, no `unsafe`, no system clock: time is passed in, so every
  result is reproducible
- The core of the Dart package
  [`timecode_tools`](https://pub.dev/packages/timecode_tools) and the Node.js
  package [`timecode-tools`](https://www.npmjs.com/package/timecode-tools)
- A C interface for other languages: `timecode-tools-ffi` in the
  [repository](https://github.com/ShowBuildLaboratories/timecode-tools-rs)

The crate does not open audio or MIDI devices. Capture with whatever you
already use (cpal, JACK, midir, a file) and pass the samples or bytes in.

## Receivers

`receiver::LtcReceiver` (PCM chunks) and `receiver::MtcReceiver` (MIDI bytes)
follow a source and report what happens as events. Both implement the
`TimecodeReceiver` trait, so the same code works for either:

```rust
use timecode_tools::{
    ltc::{PcmChunk, Samples},
    receiver::{LtcReceiver, MidiChunk, MtcReceiver, ReceiverEvent, TimecodeReceiver},
};

fn follow<R: TimecodeReceiver>(rx: &mut R, chunk: R::Chunk<'_>) -> Result<(), R::Error> {
    for event in rx.push(chunk)? {
        match event {
            ReceiverEvent::Lock(tc) => println!("lock {} {:?}", tc.label, tc.frame_rate),
            ReceiverEvent::Frame { timecode, .. } => println!("{}", timecode.label),
            ReceiverEvent::Jump(tc) => println!("jump {}", tc.label),
            ReceiverEvent::Lost => println!("lost"),
        }
    }
    Ok(())
}
# let pcm = timecode_tools::synth::generate(Default::default()).pcm;
# let captured_ns = 0;

let mut ltc = LtcReceiver::default();
follow(&mut ltc, PcmChunk {
    samples: Samples::I16(&pcm),
    sample_rate: 48_000,
    channels: 1,
    timestamp_ns: Some(captured_ns), // monotonic capture time of the first sample
})?;
# assert!(ltc.locked());

let mut mtc = MtcReceiver::default();
follow(&mut mtc, MidiChunk { data: &[0xF1, 0x00], timestamp_ns: 0 }).unwrap();
# Ok::<(), timecode_tools::ltc::LtcError>(())
```

- `Lock`: the source became stable; the first `Frame` follows.
- `Frame`: a timecode on the followed timeline, while locked, with
  `timestamp_ns` set to when it began. `ReceiverEvent::passes(target)` tells
  whether normal playback passed a target timecode (for "wait until").
- `Jump`: the timecode moved (confirmed by two contiguous frames; a single
  corrupt frame is ignored), or an MTC full-frame locate.
- `Lost`: nothing followed for `lost_after` (default 300 ms).

`frames_at(timestamp_ns)` gives the position at any moment of the input
timestamp clock (for example "now"), as a fractional frame count from a
least-squares fit of the recent signal, with the offset applied.

`ReceiverOptions` sets a fixed frame rate, an offset in frames (latency
compensation), `lost_after` and the LTC channel (default 1).

## Low-level LTC decoding

```rust
use timecode_tools::decoder::Decoder;
# use timecode_tools::synth::{generate, SynthConfig};
# let pcm: Vec<i16> = generate(SynthConfig { start_index: 90_000, frame_count: 50, ..Default::default() }).pcm;

let mut decoder = Decoder::new(48_000);
let mut first_sample = 0;
for block in pcm.chunks(480) {
    for f in decoder.feed_at(first_sample, block) {
        // f.frame.label: "01:00:00:00"; f.locked; f.estimated_fps: Some(25)
        // f.frame.sample_index: where the frame's last edge was detected
        println!("{} locked={} fps={:?}", f.frame.label, f.locked, f.estimated_fps);
    }
    first_sample += block.len() as u64;
}
assert!(decoder.locked());
```

`feed_at` takes the absolute position of the block's first sample. If you
skip samples (a dropout in your capture), pass the real position and the
decoder restarts cleanly instead of misreading across the gap. Float input
can be converted with `decoder::pcm16`.

A frame's `sample_index` is one-based and points at the sample on which its
last edge was detected. That is where the *next* frame begins, so the
timecode in the label started one frame period earlier.

## Multichannel input

```rust
use timecode_tools::session::{Channel, Session};
# use timecode_tools::synth::{generate, SynthConfig};
# let ltc = generate(SynthConfig::default()).pcm;
# let interleaved: Vec<i16> = ltc.iter().flat_map(|&s| [0, s]).collect();

// Two interleaved channels; find the one carrying LTC automatically.
let mut session = Session::new(48_000, 2, Channel::Auto);
let frames = session.feed(0, &interleaved, 0).unwrap();
assert_eq!(session.active_channel(), Some(2));
assert!(!frames.is_empty());
```

`Channel::Fixed(n)` decodes one channel only.

## Timecode arithmetic

```rust
use timecode_tools::label::{frames_per_day, Label};

let label = Label::from_index(17_982, 30, true);
assert_eq!(label.to_string(), "00:10:00;00"); // drop-frame
assert_eq!(label.frame_index(30), 17_982);
assert_eq!(frames_per_day(30, true), 2_589_408);
assert_eq!(Label::from_index(-1, 25, false).to_string(), "23:59:59:24");
```

## Timecode clock

`model::TcModel` turns decoded frames into a continuous position you can
read at any moment. It fits timecode against sample time, maps sample time
to your host clock from per-block IO stamps (removing scheduling jitter and
measuring device drift), compensates input latency, confirms jumps with two
contiguous frames, and freewheels for 8 frames when the signal stops.

```rust
use timecode_tools::{
    decoder::Decoder,
    hostmap::IoStamp,
    model::{State, TcModel},
};
# use timecode_tools::synth::{generate, SynthConfig};
# let signal = generate(SynthConfig { frame_count: 150, ..Default::default() });
# let pcm = &signal.pcm;
# let host_ns = |sample: u64| signal.host_ns(sample);

let mut decoder = Decoder::new(48_000);
let mut model = TcModel::new(48_000, 25.0);
model.set_input_state(State::NoSignal);

for (i, block) in pcm.chunks(480).enumerate() {
    let first = (i * 480) as u64;
    // The host time (monotonic ns) at which the block's first sample was
    // captured, as reported by your audio API.
    model.add_stamp(IoStamp {
        first_sample: first,
        host_ns: host_ns(first),
        frames: block.len() as u32,
        discontinuity: false,
    });
    let now = host_ns(first + block.len() as u64);
    for event in decoder.feed_at(first, block) {
        model.observe(event, now);
    }
}

let now = host_ns(pcm.len() as u64);
let snapshot = model.snapshot(now);
assert_eq!(snapshot.state, State::Locked);
// Fractional frame position at `now`, and the measured frame rate.
let position = snapshot.at(now).unwrap();
let fps = snapshot.b.unwrap() * 1e9;
assert!((fps - 25.0).abs() < 0.01);
# assert!(position > 140.0);
```

`clock::Clock` is the lower-level regression on its own.

## MIDI Time Code

```rust
use timecode_tools::mtc::{MtcDecoder, MtcKind};
# use timecode_tools::{mtc::quarter_frames, timecode::FrameRate};
# let messages = quarter_frames(FrameRate::Fps25, 90_000, 8);

let mut mtc = MtcDecoder::new();
for (bytes, arrived_ns) in messages {
    // Any MIDI bytes; MTC quarter-frame and full-frame messages are picked out.
    for reading in mtc.feed(&bytes, arrived_ns) {
        // reading.label: "01:00:00:01"; reading.rate; reading.began_ns
        assert_eq!(reading.kind, MtcKind::QuarterFrame);
        println!("{} {:?}", reading.label, reading.rate);
    }
}
```

Quarter frames give a reading every frame: at the eighth message, the
set's second frame; at the fourth message of the next set, the frame after
that, if the messages so far agree with it. `began_ns` is when the frame
started, `locked` tells that two complete sets in a row were two frames
apart, and `follows` that the reading is the frame right after the previous
one. Full-frame (SysEx) messages are reported immediately.

## Parity with the reference implementation

The repository keeps the pure-Dart implementation this crate started from as
a reference, and golden files produced by it: the decoder and clock (16
scenarios, integers and strings exactly, floats within 1e-12 relative, PCM
byte for byte) and both receivers (7 scripted LTC/MTC scenarios: every
frame, event, `waitFor` and `frames_at` result). The repository's tests
replay them against this crate, and the Dart and Node.js packages replay the
same files through their own APIs.

## License

MIT, Copyright (c) 2026 Show Build Laboratories.
