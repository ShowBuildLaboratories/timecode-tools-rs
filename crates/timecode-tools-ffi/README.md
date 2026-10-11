# timecode-tools-ffi

C interface to the `timecode-tools` core, for bindings in other languages
(the Dart package uses it). The API is in
[`include/timecode_tools.h`](include/timecode_tools.h):

- receivers (`tt_receiver_*`): LTC from PCM, MTC from MIDI bytes; each push
  returns lock / frame / jump / lost events, plus `frames_at` for the
  position at any moment;
- the low-level LTC decoder (`tt_decoder_*`);
- the regression clock with host-time mapping (`tt_clock_*`);
- timecode arithmetic (`tt_frame_index`, `tt_label_from_index`,
  `tt_frames_per_day`) and a test-signal encoder (`tt_encode_ltc`).

Handles are opaque and freed with their `*_free` function; a handle may be
used from any thread but not from two at once. Times are monotonic
nanoseconds (-1 for none). No panic crosses the boundary: an internal error
returns `TT_ERR_PANIC`.

The library is `libtimecode_tools_ffi.dylib` / `.so` / `timecode_tools_ffi.dll`.
Prebuilt copies for macOS, iOS, Android, Windows and Linux, with SHA-256
sums, are attached to each release. To build:

```sh
scripts/build-ffi.sh            # this host's targets -> target/ffi-dist/<target>/
scripts/check-c-abi.sh          # compile tests/c/smoke.c against the header and run it
```

Not published on crates.io.
