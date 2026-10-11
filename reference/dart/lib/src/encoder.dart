/// A minimal biphase-mark LTC synthesizer for tests and demos.
///
/// Produces PCM16 little-endian mono audio that [LtcDecoder] can decode.
/// It is a testing aid, not a full timecode generator: user bits and
/// polarity-correction bits are left at zero.
library;

import 'dart:typed_data';

import 'decoder.dart';

/// Synthesizes biphase-mark-encoded PCM16 little-endian mono audio for
/// [frameCount] consecutive SMPTE LTC frames starting at the given
/// timecode. A short preamble and postamble of filler bit-cells are added
/// so the very first and very last real bits have the leading/trailing
/// signal transitions a real decoder needs to measure their intervals —
/// exactly like a real LTC feed, which is never a bare single frame with no
/// surrounding signal.
Uint8List encodeLtcSequence({
  required int sampleRate,
  required int fps,
  required int startHours,
  required int startMinutes,
  required int startSeconds,
  required int startFrames,
  required int frameCount,
  bool dropFrame = false,
  int amplitude = 20000,
  int preambleBit = 1,
}) {
  final samples = <int>[];
  var level = 1;
  var emittedTarget = 0.0;
  final cellWidth = sampleRate / (fps * 80);

  void appendCell(int bit) {
    level = -level;
    if (bit == 1) {
      final half = cellWidth / 2;
      emittedTarget += half;
      _fillTo(samples, level, amplitude, emittedTarget);
      level = -level;
      emittedTarget += half;
      _fillTo(samples, level, amplitude, emittedTarget);
    } else {
      emittedTarget += cellWidth;
      _fillTo(samples, level, amplitude, emittedTarget);
    }
  }

  // [preambleBit] defaults to 1 (a run of half-cells) to match real LTC,
  // whose bitstream is never a run of constant zeros right up to the moment
  // capture starts. The decoder's period bootstrap (see the decoder's
  // top-level "Bit-cell period bootstrap" doc) buffers several intervals
  // and looks for both a half- and full-cell cluster before committing to a
  // period, so it does not matter whether this preamble happens to start
  // capture mid-half-cell or mid-full-cell — [preambleBit] lets a test
  // choose either deliberately (see the "full-cell-first bootstrap" test).
  const preambleBits = 8;
  for (var i = 0; i < preambleBits; i++) {
    appendCell(preambleBit);
  }

  var hours = startHours;
  var minutes = startMinutes;
  var seconds = startSeconds;
  var frames = startFrames;

  for (var f = 0; f < frameCount; f++) {
    final bits = _frameBits(
      hours: hours,
      minutes: minutes,
      seconds: seconds,
      frames: frames,
      dropFrame: dropFrame,
    );
    for (final bit in bits) {
      appendCell(bit);
    }
    frames++;
    if (frames >= fps) {
      frames = 0;
      seconds++;
      if (seconds >= 60) {
        seconds = 0;
        minutes++;
        if (minutes >= 60) {
          minutes = 0;
          hours++;
          if (hours >= 24) hours = 0;
        }
        // Real drop-frame timecode skips frame numbers 0 and 1 at the start
        // of every minute except every tenth — encode that skip for real
        // here rather than only setting the bitstream's dropFrame flag, so
        // a test asking for `dropFrame: true` gets an actual drop-frame
        // frame-number sequence (…;29 -> …;02) to decode.
        if (dropFrame && minutes % 10 != 0) {
          frames = 2;
        }
      }
    }
  }

  const postambleBits = 8;
  for (var i = 0; i < postambleBits; i++) {
    appendCell(0);
  }

  final bytes = Uint8List(samples.length * 2);
  final data = ByteData.view(bytes.buffer);
  for (var i = 0; i < samples.length; i++) {
    data.setInt16(i * 2, samples[i], Endian.little);
  }
  return bytes;
}

void _fillTo(List<int> samples, int level, int amplitude, double target) {
  final targetCount = target.round();
  while (samples.length < targetCount) {
    samples.add(level * amplitude);
  }
}

/// Bits of one 80-bit SMPTE 12M LTC frame, in transmission (arrival) order.
/// Only the fields [LtcDecoder] actually parses are set to real values;
/// user bits and unused flag bits are left at zero.
List<int> _frameBits({
  required int hours,
  required int minutes,
  required int seconds,
  required int frames,
  required bool dropFrame,
}) {
  final bits = List<int>.filled(80, 0);
  void setBcd(int start, int count, int value) {
    for (var k = 0; k < count; k++) {
      bits[start + k] = (value >> k) & 1;
    }
  }

  setBcd(0, 4, frames % 10);
  setBcd(8, 2, frames ~/ 10);
  bits[10] = dropFrame ? 1 : 0;
  setBcd(16, 4, seconds % 10);
  setBcd(24, 3, seconds ~/ 10);
  setBcd(32, 4, minutes % 10);
  setBcd(40, 3, minutes ~/ 10);
  setBcd(48, 4, hours % 10);
  setBcd(56, 2, hours ~/ 10);

  final sync = LtcDecoder.syncWordBits.split('').map(int.parse).toList();
  for (var i = 0; i < 16; i++) {
    bits[64 + i] = sync[i];
  }
  return bits;
}
