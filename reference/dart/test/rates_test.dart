import 'dart:typed_data';

import 'package:timecode_tools_reference/timecode_tools_reference.dart';
import 'package:test/test.dart';

/// Pushes [pcm] in 10 ms chunks and collects every decoded frame.
List<LtcFrame> decodeAll(LtcDecoder decoder, Uint8List pcm) {
  final out = <LtcFrame>[];
  final chunk = (decoder.sampleRate ~/ 100) * 2;
  for (var i = 0; i < pcm.length; i += chunk) {
    final end = i + chunk < pcm.length ? i + chunk : pcm.length;
    out.addAll(decoder.pushBytes(Uint8List.sublistView(pcm, i, end)));
  }
  return out;
}

void main() {
  const cases = [
    FrameRate.fps24,
    FrameRate.fps25,
    FrameRate.fps2997Drop,
    FrameRate.fps30,
  ];
  for (final rate in cases) {
    for (final sampleRate in const [44100, 48000]) {
      test('${rate.label} fps at $sampleRate Hz: decodes, locks, detects rate',
          () {
        final pcm = encodeLtcSequence(
          sampleRate: sampleRate,
          fps: rate.nominal,
          dropFrame: rate.dropFrame,
          startHours: 10,
          startMinutes: 0,
          startSeconds: 58,
          startFrames: 0,
          frameCount: rate.nominal * 4, // crosses a minute boundary
        );
        final decoder = LtcDecoder(sampleRate: sampleRate);
        final frames = decodeAll(decoder, pcm);
        expect(frames.length, greaterThanOrEqualTo(rate.nominal * 4 - 1));
        int index(LtcFrame f) => ltcFrameIndex(
              hours: f.hours,
              minutes: f.minutes,
              seconds: f.seconds,
              frames: f.frames,
              dropFrame: f.dropFrame,
              fps: rate.nominal,
            );
        for (var i = 1; i < frames.length; i++) {
          expect(index(frames[i]) - index(frames[i - 1]), 1,
              reason: '${frames[i - 1]} -> ${frames[i]}');
        }
        expect(frames.every((f) => f.dropFrame == rate.dropFrame), isTrue);
        expect(decoder.locked, isTrue);
        expect(
          FrameRate.fromDecoded(
              fps: decoder.estimatedFps, dropFrame: frames.last.dropFrame),
          rate,
        );
      });
    }
  }
}
