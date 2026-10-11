# timecode-tools-rs

The Rust core of **timecode-tools**: SMPTE LTC (linear timecode) and MIDI
Time Code readers, by [Show Build Laboratories](https://github.com/ShowBuildLaboratories).

| Package | Repository | Install |
| --- | --- | --- |
| Rust crate [`timecode-tools`](crates/timecode-tools) | this one | `cargo add timecode-tools` |
| Dart / Flutter package `timecode_tools` | [timecode-tools-dart](https://github.com/ShowBuildLaboratories/timecode-tools-dart) | `dart pub add timecode_tools` |
| Node.js package `timecode-tools` | [timecode-tools-node](https://github.com/ShowBuildLaboratories/timecode-tools-node) | `npm install timecode-tools` |

The Dart and Node.js packages call this core, so all three give the same
answers.

- LTC from PCM: 24, 25, 29.97 drop-frame and 30 fps, detected automatically;
  any sample rate, either polarity, multichannel
- MTC from raw MIDI bytes: quarter-frame and full-frame messages
- Receivers that report lock, frames, jumps (confirmed by two frames; a
  single corrupt frame is ignored) and loss, with latency offset and a
  least-squares clock for the position at any moment
- No dependencies, no `unsafe` in the core, no device code: you pass samples
  or bytes in

See the [crate README](crates/timecode-tools/README.md) for the Rust API.

## Layout

```
crates/timecode-tools/       The core crate (crates.io: timecode-tools)
crates/timecode-tools-ffi/   C interface (include/timecode_tools.h) for other languages
reference/dart/              The pure-Dart reference implementation (not published)
goldens/                     Golden files produced by the reference
  decoder-clock/             Decoder and clock: 16 scenarios
  receiver/                  LtcReceiver and MtcReceiver: 7 scripted scenarios
scripts/                     Building the C library, checking the C interface
```

## Golden files

The pure-Dart implementation in `reference/dart` is the reference. Its
results on scripted inputs are stored in `goldens/`, and the tests replay
them:

- here, against the Rust core (`cargo test`);
- in the Dart and Node.js repositories, through their public APIs (they keep
  a copy of `goldens/` from the core version they are built with).

`reference/dart` checks that the committed files still match it:

```sh
cd reference/dart && dart pub get && dart test
dart run tool/receiver_goldens.dart ../../goldens/receiver   # regenerate
```

## C interface

`crates/timecode-tools-ffi` builds `libtimecode_tools_ffi` (`.dylib`, `.so`,
`.dll`) with the API in
[`include/timecode_tools.h`](crates/timecode-tools-ffi/include/timecode_tools.h):
receivers, the low-level decoder, the clock, timecode arithmetic and a test
signal encoder. Prebuilt libraries for each platform are attached to each
[GitHub release](https://github.com/ShowBuildLaboratories/timecode-tools-rs/releases)
with SHA-256 sums; the Dart package uses them.

```sh
scripts/build-ffi.sh                  # this host's targets -> target/ffi-dist/<target>/
scripts/build-ffi.sh aarch64-apple-ios aarch64-linux-android
scripts/check-c-abi.sh                # compile a C program against the header and run it
```

## Building and testing

Requires Rust 1.88 or later (and the Dart SDK for `reference/dart`).

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT, Copyright (c) 2026 Show Build Laboratories. See [LICENSE](LICENSE).
