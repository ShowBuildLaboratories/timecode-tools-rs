import 'dart:async';
import 'dart:typed_data';

import 'package:test/test.dart';
import 'package:timecode_tools_reference/timecode_tools_reference.dart';

import 'support/mtc_signal.dart';

/// Code written against the interface works with either receiver.
Future<List<Timecode>> collect<T>(
  TimecodeReceiver<T> rx,
  List<T> input,
) async {
  final got = <Timecode>[];
  final events = <StateEventType>[];
  rx.frames.listen(got.add);
  rx.events.listen((e) => events.add(e.type));
  final c = StreamController<T>();
  rx.attach(c.stream);
  input.forEach(c.add);
  await Future<void>.delayed(Duration.zero);
  expect(rx.isLocked, isTrue);
  expect(events, [StateEventType.lock]);
  await rx.close();
  await c.close();
  return got;
}

void main() {
  test('LtcReceiver and MtcReceiver share TimecodeReceiver', () async {
    final bytes = encodeLtcSequence(
      sampleRate: 48000,
      fps: 25,
      startHours: 1,
      startMinutes: 0,
      startSeconds: 0,
      startFrames: 0,
      frameCount: 50,
    );
    final samples = Int16List.view(bytes.buffer, 0, bytes.length ~/ 2);
    final pcm = <PcmChunk>[
      for (var i = 0; i < samples.length; i += 960)
        PcmChunk(
          samples: Int16List.sublistView(
              samples, i, i + 960 < samples.length ? i + 960 : samples.length),
          sampleRate: 48000,
          timestamp: Duration(microseconds: i * 1000000 ~/ 48000),
        ),
    ];
    final TimecodeReceiver<PcmChunk> ltc = LtcReceiver();
    final TimecodeReceiver<MidiChunk> mtc = MtcReceiver();
    final a = await collect(ltc, pcm);
    final b = await collect(
        mtc, quarterFrames(const Timecode(1, 0, 0, 0), FrameRate.fps25, 50));
    expect(a.last.hours, 1);
    expect(b.last.hours, 1);
    expect(ltc.detectedRate, FrameRate.fps25);
    expect(mtc.detectedRate, FrameRate.fps25);
  });
}
