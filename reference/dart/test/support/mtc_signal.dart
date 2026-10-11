// Generates MTC (MIDI timecode) byte streams for tests.
import 'dart:typed_data';

import 'package:timecode_tools_reference/timecode_tools_reference.dart';

int rateBits(FrameRate rate) => switch (rate) {
      FrameRate.fps24 => 0,
      FrameRate.fps25 => 1,
      FrameRate.fps2997Drop => 2,
      _ => 3,
    };

/// Quarter-frame messages for [frameCount] frames (a multiple of 2) from
/// [start], one [MidiChunk] per message, timestamped from [t0] at four
/// messages per frame.
List<MidiChunk> quarterFrames(
  Timecode start,
  FrameRate rate,
  int frameCount, {
  Duration t0 = const Duration(seconds: 10),
  bool interleaveClock = false,
}) {
  final out = <MidiChunk>[];
  final quarter = 1e6 / rate.fps / 4;
  var index = start.frameIndex(rate);
  var n = 0;
  for (var f = 0; f < frameCount; f += 2) {
    final tc = Timecode.fromFrameIndex(index, rate);
    final pieces = [
      tc.frames & 0x0F,
      tc.frames >> 4 & 0x01,
      tc.seconds & 0x0F,
      tc.seconds >> 4 & 0x03,
      tc.minutes & 0x0F,
      tc.minutes >> 4 & 0x03,
      tc.hours & 0x0F,
      (tc.hours >> 4 & 0x01) | rateBits(rate) << 1,
    ];
    for (var p = 0; p < 8; p++) {
      final bytes = <int>[
        if (interleaveClock) 0xF8,
        0xF1,
        if (interleaveClock) 0xFE,
        p << 4 | pieces[p],
      ];
      out.add(MidiChunk(
        Uint8List.fromList(bytes),
        timestamp: t0 + Duration(microseconds: (n * quarter).round()),
      ));
      n++;
    }
    index += 2;
  }
  return out;
}

/// A full-frame (locate) SysEx message for [tc].
Uint8List fullFrame(Timecode tc, FrameRate rate, {int device = 0x7F}) =>
    Uint8List.fromList([
      0xF0,
      0x7F,
      device,
      0x01,
      0x01,
      rateBits(rate) << 5 | tc.hours,
      tc.minutes,
      tc.seconds,
      tc.frames,
      0xF7,
    ]);
