# timecode_tools_reference

The pure-Dart implementation that `timecode-tools` started from. It is kept
as the reference for parity tests and is not published.

- `tool/receiver_goldens.dart` runs scripted LTC and MTC inputs through
  `LtcReceiver` and `MtcReceiver` and writes what they report to
  `../../goldens/receiver/` (frames, lock / lost / jump, `waitFor`,
  `framesAt`).
- `test/receiver_goldens_test.dart` checks that the committed goldens still
  match this implementation.

```sh
dart pub get
dart test
dart run tool/receiver_goldens.dart ../../goldens/receiver   # regenerate
```

The Rust core (`crates/timecode-tools/tests/receiver_goldens.rs`) and the
Dart and Node.js packages replay the same files and must report the same
results.
