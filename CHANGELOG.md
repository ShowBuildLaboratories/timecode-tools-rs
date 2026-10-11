# Changelog

This project follows [Semantic Versioning](https://semver.org/). The Dart
and Node.js packages use the same version numbers as the core they are built
with.

## 0.1.0 - Unreleased

First public release.

- LTC decoder for 16-bit PCM: biphase-mark decoding, automatic polarity,
  lock detection and nominal frame-rate estimation (24, 25, 30; drop-frame
  flag for 29.97).
- `LtcReceiver` (PCM chunks) and `MtcReceiver` (MIDI bytes) behind the
  `TimecodeReceiver` trait: lock, frame, jump (two-frame confirmation) and
  loss events, latency offset, `passes()` for waiting on a timecode, and
  `frames_at()` for the position at any moment.
- MTC reader for raw MIDI bytes: quarter-frame (a reading every frame) and
  full-frame messages, skipping other MIDI traffic, running status and
  real-time bytes.
- Timecode labels, frame rates, frame-index arithmetic and drop-frame rules.
- Regression clock (`Clock`), host-time mapping (`HostMap`) and a combined
  timecode clock (`TcModel`) with freewheel and jam confirmation.
- Multichannel `Session` with fixed or automatic channel selection.
- `timecode-tools-ffi`: a C interface (`timecode_tools.h`) with prebuilt
  libraries for macOS, iOS, Android, Windows and Linux.
- Results match the pure-Dart reference implementation on 23 golden
  scenarios.
- No dependencies; `#![forbid(unsafe_code)]` in the core.
