import 'dart:async';
import 'dart:typed_data';

import 'package:timecode_tools_reference/timecode_tools_reference.dart';
import 'package:test/test.dart';

import 'support/mtc_signal.dart';

Future<void> settle() => Future<void>.delayed(Duration.zero);

StreamController<MidiChunk> feed(MtcReceiver rx, List<MidiChunk> list) {
  final c = StreamController<MidiChunk>();
  rx.attach(c.stream);
  list.forEach(c.add);
  return c;
}

void main() {
  for (final rate in [
    FrameRate.fps24,
    FrameRate.fps25,
    FrameRate.fps2997Drop,
    FrameRate.fps30,
  ]) {
    test('${rate.label}: quarter frames give one frame per frame, locked',
        () async {
      final rx = MtcReceiver();
      final got = <Timecode>[];
      final events = <StateEventType>[];
      rx.frames.listen(got.add);
      rx.events.listen((e) => events.add(e.type));
      // 00:00:58:00 for 4 s crosses a minute boundary (drop-frame skips).
      feed(rx,
          quarterFrames(const Timecode(0, 0, 58, 0), rate, rate.nominal * 4));
      await settle();
      expect(rx.detectedRate, rate);
      expect(events, [StateEventType.lock]);
      // Frames start at the lock: the third reading (start + 3), once two
      // complete sets in a row were two frames apart.
      expect(
          got.first,
          Timecode.fromFrameIndex(
              const Timecode(0, 0, 58, 0).frameIndex(rate) + 3, rate));
      for (var i = 1; i < got.length; i++) {
        expect(got[i].frameIndex(rate) - got[i - 1].frameIndex(rate), 1,
            reason: '${got[i - 1]} -> ${got[i]}');
        expect(got[i].dropFrame, rate.dropFrame);
      }
      // N frames of quarter frames give N - 1 readings (the first set's
      // first frame is only known once the set is complete), of which the
      // first two come before the lock.
      expect(got.length, rate.nominal * 4 - 3);
      // Each frame's timestamp is when it began (four quarter frames each).
      final dt = got.last.timestamp! - got[got.length - 2].timestamp!;
      expect(dt.inMicroseconds, closeTo(1e6 / rate.fps, 2));
      await rx.close();
    });
  }

  test('frame timestamps are the frame starts', () async {
    const t0 = Duration(seconds: 10);
    final rx = MtcReceiver();
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    feed(rx, quarterFrames(Timecode.zero, FrameRate.fps25, 10, t0: t0));
    await settle();
    for (final tc in got) {
      final expected = t0 +
          Duration(
              microseconds: (tc.frameIndex(FrameRate.fps25) * 40000).round());
      expect(tc.timestamp, expected, reason: '$tc');
    }
    await rx.close();
  });

  test('positionAt after lock', () async {
    final rx = MtcReceiver();
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    feed(rx, quarterFrames(Timecode.zero, FrameRate.fps30, 30));
    await settle();
    expect(rx.isLocked, isTrue);
    expect(rx.positionAt(got.last.timestamp!), got.last);
    await rx.close();
  });

  test('ignores interleaved real-time bytes and other messages', () async {
    final rx = MtcReceiver();
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    final list = [
      MidiChunk(Uint8List.fromList([0x90, 0x40, 0x7F, 0x40, 0x00])),
      ...quarterFrames(Timecode.zero, FrameRate.fps25, 10,
          interleaveClock: true),
    ];
    feed(rx, list);
    await settle();
    expect(got.length, 7);
    expect(rx.isLocked, isTrue);
    await rx.close();
  });

  test('a full-frame message ends the chase and is reported as a jump',
      () async {
    final rx = MtcReceiver();
    final got = <Timecode>[];
    final events = <StateEventType>[];
    rx.frames.listen(got.add);
    rx.events.listen((e) => events.add(e.type));
    final c = feed(rx, quarterFrames(Timecode.zero, FrameRate.fps25, 10));
    await settle();
    // Split the SysEx across two chunks.
    final ff = fullFrame(const Timecode(1, 0, 0, 0), FrameRate.fps25);
    c.add(MidiChunk(Uint8List.sublistView(ff, 0, 4)));
    c.add(MidiChunk(Uint8List.sublistView(ff, 4)));
    await settle();
    expect(rx.current, Timecode.parse('01:00:00:00'));
    expect(got.last, Timecode.parse('00:00:00:09'));
    expect(events,
        [StateEventType.lock, StateEventType.lost, StateEventType.jump]);
    expect(rx.isLocked, isFalse);
    // Quarter frames from the new place lock again.
    for (final m in quarterFrames(
        const Timecode(1, 0, 0, 0), FrameRate.fps25, 10,
        t0: const Duration(seconds: 30))) {
      c.add(m);
    }
    await settle();
    expect(events.last, StateEventType.lock);
    await rx.close();
  });

  test('a full-frame message carries the rate', () async {
    final rx = MtcReceiver();
    final c = feed(rx, const []);
    c.add(MidiChunk(
        fullFrame(Timecode.parse('10:00:00;02'), FrameRate.fps2997Drop)));
    await settle();
    expect(rx.detectedRate, FrameRate.fps2997Drop);
    expect(rx.current.toString(), '10:00:00;02');
    await rx.close();
  });

  test('lost when quarter frames stop', () async {
    // lostAfter must be longer than a frame (40 ms at 25 fps).
    final rx = MtcReceiver(lostAfter: const Duration(milliseconds: 100));
    final events = <StateEventType>[];
    rx.events.listen((e) => events.add(e.type));
    feed(rx, quarterFrames(Timecode.zero, FrameRate.fps25, 10));
    await settle();
    expect(events, [StateEventType.lock]);
    await Future<void>.delayed(const Duration(milliseconds: 250));
    expect(events, [StateEventType.lock, StateEventType.lost]);
    await rx.close();
  });

  test('out-of-order pieces do not produce frames', () async {
    final rx = MtcReceiver();
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    final list = quarterFrames(Timecode.zero, FrameRate.fps25, 2);
    feed(rx, list.reversed.toList());
    await settle();
    expect(got, isEmpty);
    await rx.close();
  });

  test('offsetFrames and waitFor work as for LTC', () async {
    final rx = MtcReceiver(offsetFrames: 3);
    final target = Timecode.parse('00:00:01:00');
    final done = rx.waitFor(target);
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    feed(rx, quarterFrames(Timecode.zero, FrameRate.fps25, 50));
    expect(await done, target);
    expect(got.first, Timecode.parse('00:00:00:06')); // 3 + offset 3
    await rx.close();
  });
}
