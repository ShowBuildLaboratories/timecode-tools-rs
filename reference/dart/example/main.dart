// Reads LTC from a synthesized signal, and MTC from MIDI bytes.
import 'dart:typed_data';

import 'package:timecode_tools_reference/timecode_tools_reference.dart';

Future<void> main() async {
  const sampleRate = 48000;

  // Two seconds of 25 fps LTC starting at 01:00:00:00 (PCM16, mono).
  final bytes = encodeLtcSequence(
    sampleRate: sampleRate,
    fps: 25,
    startHours: 1,
    startMinutes: 0,
    startSeconds: 0,
    startFrames: 0,
    frameCount: 50,
  );
  final samples = Int16List.view(bytes.buffer, 0, bytes.length ~/ 2);

  // In an app this stream comes from your audio capture, in small chunks,
  // each stamped with the capture time of its first sample.
  Stream<PcmChunk> capture() async* {
    const chunk = sampleRate ~/ 50; // 20 ms
    for (var i = 0; i < samples.length; i += chunk) {
      final end = i + chunk < samples.length ? i + chunk : samples.length;
      yield PcmChunk(
        samples: Int16List.sublistView(samples, i, end),
        sampleRate: sampleRate,
        timestamp: Duration(microseconds: i * 1000000 ~/ sampleRate),
      );
    }
  }

  final rx = LtcReceiver(frameRate: FrameRate.auto, offsetFrames: 0);
  rx.events.listen((e) => print('event: $e'));
  rx.frames.where((tc) => tc.frames == 0).listen((tc) => print(tc));

  final passed = rx.waitFor(Timecode.parse('01:00:01:00'));
  rx.attach(capture());
  print('passed ${await passed} at rate ${rx.detectedRate?.label}');
  await Future<void>.delayed(const Duration(milliseconds: 10));
  await rx.close();

  // The low-level decoder, without a stream:
  final decoder = LtcDecoder(sampleRate: sampleRate);
  final frames = decoder.push(samples);
  print('decoded ${frames.length} frames, first ${frames.first}');

  // MTC: a full-frame (locate) message for 10:00:00:00 at 25 fps, as raw
  // MIDI bytes. In an app the stream comes from a MIDI input.
  final mtc = MtcReceiver();
  final located = mtc.frames.first;
  mtc.attach(Stream.value(MidiChunk(Uint8List.fromList(
      [0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x20 | 10, 0, 0, 0, 0xF7]))));
  print('MTC located at ${await located} (${mtc.detectedRate?.label} fps)');
  await mtc.close();
}
