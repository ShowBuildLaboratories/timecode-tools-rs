import 'dart:math';
import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:timecode_tools_reference/timecode_tools_reference.dart';

void main() {
  group('LtcDecoder', () {
    test('decodes an ascending 25fps sequence fed in uneven chunks', () async {
      const sampleRate = 48000;
      const fps = 25;
      const frameCount = 10;
      final bytes = encodeLtcSequence(
        sampleRate: sampleRate,
        fps: fps,
        startHours: 1,
        startMinutes: 2,
        startSeconds: 3,
        startFrames: 4,
        frameCount: frameCount,
      );

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      received.addAll(_feedInUnevenChunks(decoder, bytes, seed: 7));

      final expected = List<LtcFrame>.generate(
        frameCount,
        (i) => const LtcFrame(
          hours: 1,
          minutes: 2,
          seconds: 3,
          frames: 4,
          dropFrame: false,
        ).copyWithFrames(4 + i),
      );

      expect(received, expected);
      expect(decoder.locked, isTrue);
      expect(decoder.estimatedFps, 25);
    });

    test('pure silence never emits a frame', () async {
      const sampleRate = 48000;
      final silence = Uint8List(sampleRate * 2); // 1 second, all zero

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      received.addAll(decoder.pushBytes(silence));

      expect(received, isEmpty);
      expect(decoder.locked, isFalse);
      expect(decoder.signalLevel, 0);
    });

    test('white noise never emits a frame', () async {
      const sampleRate = 48000;
      // Fixed seed: deterministic, not flaky. ~0.2s of full-range noise.
      final noise = _randomPcm16(sampleRate ~/ 5, seed: 12345);

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      received.addAll(decoder.pushBytes(noise));

      expect(received, isEmpty);
      expect(decoder.locked, isFalse);
    });

    test('decodes correctly with inverted signal polarity', () async {
      const sampleRate = 48000;
      const fps = 25;
      const frameCount = 10;
      final bytes = encodeLtcSequence(
        sampleRate: sampleRate,
        fps: fps,
        startHours: 1,
        startMinutes: 2,
        startSeconds: 3,
        startFrames: 4,
        frameCount: frameCount,
      );
      final inverted = _invertPcm16(bytes);

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      received.addAll(_feedInUnevenChunks(decoder, inverted, seed: 3));

      final expected = List<LtcFrame>.generate(
        frameCount,
        (i) => const LtcFrame(
          hours: 1,
          minutes: 2,
          seconds: 3,
          frames: 4,
          dropFrame: false,
        ).copyWithFrames(4 + i),
      );

      expect(received, expected);
      expect(decoder.locked, isTrue);
    });

    test('push carries a leftover byte across an odd-length chunk split',
        () async {
      const sampleRate = 48000;
      const fps = 25;
      final bytes = encodeLtcSequence(
        sampleRate: sampleRate,
        fps: fps,
        startHours: 0,
        startMinutes: 0,
        startSeconds: 0,
        startFrames: 0,
        frameCount: 3,
      );

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      // Force a split at an odd byte offset so a sample is torn in half
      // across two push calls.
      final splitPoint = (bytes.length ~/ 2) | 1;
      received.addAll(
          decoder.pushBytes(Uint8List.sublistView(bytes, 0, splitPoint)));
      received
          .addAll(decoder.pushBytes(Uint8List.sublistView(bytes, splitPoint)));

      expect(received, isNotEmpty);
      expect(received.first.frames, 0);
    });

    test(
        'sampleIndex is monotonic and tracks real elapsed samples across '
        'chunk boundaries', () async {
      const sampleRate = 48000;
      const fps = 25;
      const frameCount = 6;
      final bytes = encodeLtcSequence(
        sampleRate: sampleRate,
        fps: fps,
        startHours: 0,
        startMinutes: 0,
        startSeconds: 0,
        startFrames: 0,
        frameCount: frameCount,
      );

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      // Small, uneven chunk sizes (including mid-sample splits) so a
      // frame's sync word can straddle a push call boundary — exactly
      // the case sampleIndex must stay correct across, since the decoder's
      // sample counter must not reset or double-count at a chunk seam.
      received.addAll(_feedInUnevenChunks(decoder, bytes, seed: 42));

      expect(received.length, frameCount);

      final totalSamples = bytes.length ~/ 2;
      const framePeriod = sampleRate / fps; // exactly 1920 samples/frame
      var previous = -1;
      for (var i = 0; i < received.length; i++) {
        final sample = received[i].sampleIndex;
        expect(sample, greaterThan(previous));
        expect(sample, lessThanOrEqualTo(totalSamples));
        if (i > 0) {
          expect(
            (sample - previous).toDouble(),
            closeTo(framePeriod, framePeriod * 0.05),
          );
        }
        previous = sample;
      }
      // The decoder's own running counter never falls behind the last
      // frame it reported.
      expect(decoder.sampleIndex, greaterThanOrEqualTo(previous));
    });

    test(
        'drop-frame lock stays continuous across a non-tenth-minute '
        'boundary (…;29 -> …;02)', () async {
      // Regression test: the old lock-contiguity check computed each
      // frame's "total frame count" with naive seconds*fps+frames math,
      // which is wrong for drop-frame at any minute boundary that isn't a
      // multiple of 10 — the frame-number field jumps …;29 -> …;02 there
      // (drop-frame skips 0 and 1), and naive math reads that as a false
      // +3 jump, breaking contiguity and dropping lock for a few frames
      // every such minute.
      const sampleRate = 48000;
      const fps = 30;
      // 00:00:58;00 .. through 00:01:01;01 — comfortably locks well before
      // crossing into minute 1, and keeps decoding well after it too.
      const frameCount = 90;
      final bytes = encodeLtcSequence(
        sampleRate: sampleRate,
        fps: fps,
        startHours: 0,
        startMinutes: 0,
        startSeconds: 58,
        startFrames: 0,
        frameCount: frameCount,
        dropFrame: true,
      );

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      // Feed frame-by-frame (after the preamble) so `decoder.locked` can be
      // sampled right after each individual frame is decoded — feeding the
      // whole buffer in one call would only let us observe the *final*
      // lock state, which would look fine in both the buggy and the fixed
      // decoder (there are plenty of good frames after the boundary to
      // re-lock by the end either way). Both `preambleBits` and the
      // per-frame byte length below are exact for 48kHz/30fps (1600
      // samples/frame), matching `encodeLtcSequence`'s own bit-cell math.
      const preambleBits = 8;
      const cellWidth = sampleRate / (fps * 80);
      final preambleBytes = (preambleBits * cellWidth * 2).round();
      final frameBytes = ((sampleRate / fps) * 2).round();

      // `lockedAfterFrame[k]` is `decoder.locked` sampled right after
      // feeding iteration k's chunk. Biphase mark code signals a bit
      // cell's *closing* edge with the start-of-cell transition of
      // whatever comes right after it, so frame `k`'s own sync word does
      // not actually finish decoding until iteration `k + 1`'s chunk
      // supplies that next transition — i.e. `lockedAfterFrame[k]` reflects
      // the lock state right after frame `k - 1` (not frame `k`) was
      // decoded. `decoder.locked` is updated synchronously inside the push
      // call, so it is safe to sample immediately.
      received.addAll(
          decoder.pushBytes(Uint8List.sublistView(bytes, 0, preambleBytes)));
      final lockedAfterFrame = <bool>[];
      var offset = preambleBytes;
      for (var i = 0; i < frameCount; i++) {
        final end = (offset + frameBytes).clamp(0, bytes.length);
        received.addAll(
            decoder.pushBytes(Uint8List.sublistView(bytes, offset, end)));
        lockedAfterFrame.add(decoder.locked);
        offset = end;
      }
      // Biphase mark code signals a bit cell's *closing* edge with the
      // start-of-cell transition of whatever comes next — so the very last
      // real frame's sync word only actually completes once at least the
      // first sample of the postamble that follows it has been seen. Feed
      // the remainder (the postamble) so that last frame is not lost to
      // this end-of-buffer edge effect; it isn't near the boundary this
      // test cares about, so it doesn't affect `lockedAfterFrame` above.
      received.addAll(decoder
          .pushBytes(Uint8List.sublistView(bytes, offset, bytes.length)));

      expect(received.length, frameCount);

      final boundaryIndex = received.indexWhere(
        (f) => f.minutes == 1 && f.seconds == 0 && f.frames == 2,
      );
      expect(boundaryIndex, greaterThan(0));
      final justBefore = received[boundaryIndex - 1];
      expect(justBefore.minutes, 0);
      expect(justBefore.seconds, 59);
      expect(justBefore.frames, 29);

      // Locked right up to the boundary (immediately after the last frame
      // before it, 00:00:59;29, was decoded)...
      expect(lockedAfterFrame[boundaryIndex], isTrue);
      // ...and stays locked immediately *after* the boundary frame itself
      // (00:01:00;02) decodes — this is the assertion that fails under the
      // naive (non-drop-frame-aware) contiguity math, which sees the ;29 ->
      // ;02 skip as a false +3 jump and resets `_consecutiveContiguous`
      // right here.
      expect(lockedAfterFrame[boundaryIndex + 1], isTrue);
    });

    test(
        'a two-hypothesis bootstrap still locks when capture starts on a '
        'full cell instead of a half cell', () async {
      // Regression test: the old bootstrap assumed the very first measured
      // interval was a half-cell and doubled it to seed the period
      // estimate. If capture instead starts on a full cell (this test's
      // `preambleBit: 0`), that guess is 2x too large — and because the
      // classifier's threshold is derived from the period estimate itself,
      // every real full-cell interval that follows also reads as "half"
      // against the inflated threshold, gets paired with the next interval
      // into a `fullSpan` that reinforces the same wrong 2x period, and the
      // decoder harmonically mislocks onto double the true period instead
      // of self-correcting.
      const sampleRate = 48000;
      const fps = 25;
      const frameCount = 10;
      final bytes = encodeLtcSequence(
        sampleRate: sampleRate,
        fps: fps,
        startHours: 3,
        startMinutes: 4,
        startSeconds: 5,
        startFrames: 6,
        frameCount: frameCount,
        preambleBit: 0, // full cells, not the usual half-cell run
      );

      final decoder = LtcDecoder(sampleRate: sampleRate);
      final received = <LtcFrame>[];

      received.addAll(_feedInUnevenChunks(decoder, bytes, seed: 99));

      final expected = List<LtcFrame>.generate(
        frameCount,
        (i) => const LtcFrame(
          hours: 3,
          minutes: 4,
          seconds: 5,
          frames: 6,
          dropFrame: false,
        ).copyWithFrames(6 + i),
      );

      expect(received, expected);
      expect(decoder.locked, isTrue);
      expect(decoder.estimatedFps, 25);
    });
  });
}

extension on LtcFrame {
  LtcFrame copyWithFrames(int frames) => LtcFrame(
        hours: hours,
        minutes: minutes,
        seconds: seconds,
        frames: frames,
        dropFrame: dropFrame,
      );
}

/// Feeds [bytes] into [decoder] in small, uneven chunk sizes (including
/// splits that fall mid-sample) to exercise carry-across-push handling.
List<LtcFrame> _feedInUnevenChunks(
  LtcDecoder decoder,
  Uint8List bytes, {
  int seed = 1,
}) {
  final random = Random(seed);
  final out = <LtcFrame>[];
  var offset = 0;
  while (offset < bytes.length) {
    final chunk = 1 + random.nextInt(97);
    final end = (offset + chunk).clamp(0, bytes.length);
    out.addAll(decoder.pushBytes(Uint8List.sublistView(bytes, offset, end)));
    offset = end;
  }
  return out;
}

Uint8List _randomPcm16(int sampleCount, {required int seed}) {
  final random = Random(seed);
  final bytes = Uint8List(sampleCount * 2);
  final data = ByteData.view(bytes.buffer);
  for (var i = 0; i < sampleCount; i++) {
    final sample = random.nextInt(65536) - 32768;
    data.setInt16(i * 2, sample, Endian.little);
  }
  return bytes;
}

Uint8List _invertPcm16(Uint8List bytes) {
  final data = ByteData.sublistView(bytes);
  final out = Uint8List(bytes.length);
  final outData = ByteData.sublistView(out);
  final sampleCount = bytes.length ~/ 2;
  for (var i = 0; i < sampleCount; i++) {
    final sample = data.getInt16(i * 2, Endian.little);
    outData.setInt16(i * 2, -sample, Endian.little);
  }
  return out;
}
