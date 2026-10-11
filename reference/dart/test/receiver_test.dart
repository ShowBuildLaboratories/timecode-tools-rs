import 'dart:async';
import 'dart:typed_data';

import 'package:timecode_tools_reference/timecode_tools_reference.dart';
import 'package:test/test.dart';

Int16List ltcSamples(
  FrameRate rate, {
  int sampleRate = 48000,
  Timecode start = const Timecode(1, 0, 0, 0),
  int seconds = 2,
}) {
  final bytes = encodeLtcSequence(
    sampleRate: sampleRate,
    fps: rate.nominal,
    dropFrame: rate.dropFrame,
    startHours: start.hours,
    startMinutes: start.minutes,
    startSeconds: start.seconds,
    startFrames: start.frames,
    frameCount: rate.nominal * seconds,
  );
  return Int16List.view(bytes.buffer, bytes.offsetInBytes, bytes.length ~/ 2);
}

/// Splits [mono] into 20 ms chunks with exact timestamps from [t0].
List<PcmChunk> chunks(
  Int16List mono, {
  int sampleRate = 48000,
  Duration t0 = const Duration(seconds: 100),
  int channels = 1,
}) {
  final per = sampleRate ~/ 50 * channels;
  return [
    for (var i = 0; i < mono.length; i += per)
      PcmChunk(
        samples: Int16List.sublistView(
            mono, i, i + per < mono.length ? i + per : mono.length),
        sampleRate: sampleRate,
        channels: channels,
        timestamp: t0 +
            Duration(microseconds: (i ~/ channels * 1e6 / sampleRate).round()),
      ),
  ];
}

Future<void> settle() => Future<void>.delayed(Duration.zero);

/// Attaches [list] as a live stream that stays open (a finished stream
/// counts as the signal going away).
StreamController<PcmChunk> feed(LtcReceiver rx, List<PcmChunk> list) {
  final c = StreamController<PcmChunk>();
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
    test('${rate.label}: auto-detects, locks once, frames advance by one',
        () async {
      final rx = LtcReceiver();
      final got = <Timecode>[];
      final statuses = <StateEventType>[];
      rx.frames.listen(got.add);
      rx.events.listen((e) => statuses.add(e.type));
      feed(rx, chunks(ltcSamples(rate)));
      await settle();
      expect(rx.detectedRate, rate);
      expect(statuses, [StateEventType.lock]);
      expect(got.length, greaterThan(rate.nominal));
      for (var i = 1; i < got.length; i++) {
        expect(got[i].frameIndex(rate) - got[i - 1].frameIndex(rate), 1);
        expect(got[i].dropFrame, rate.dropFrame);
      }
      // Frame timestamps are one frame apart.
      final dt = got.last.timestamp! - got[got.length - 2].timestamp!;
      expect(dt.inMicroseconds, closeTo(1e6 / rate.fps, 1e6 / rate.fps * 0.03));
      await rx.close();
    });
  }

  test('positionAt matches the frame timestamps', () async {
    final rx = LtcReceiver();
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    feed(rx, chunks(ltcSamples(FrameRate.fps25)));
    await settle();
    final last = got.last;
    final f = rx.framesAt(last.timestamp!)!;
    expect(f, closeTo(last.frameIndex(FrameRate.fps25).toDouble(), 0.25));
    expect(rx.positionAt(last.timestamp!), last);
    await rx.close();
  });

  test('offsetFrames is added to every timecode', () async {
    Future<List<Timecode>> run(int offset) async {
      final rx = LtcReceiver(offsetFrames: offset);
      final got = <Timecode>[];
      rx.frames.listen(got.add);
      feed(rx, chunks(ltcSamples(FrameRate.fps25)));
      await settle();
      await rx.close();
      return got;
    }

    final a = await run(0);
    final b = await run(2);
    final c = await run(-3);
    expect(
        b.last.frameIndex(FrameRate.fps25) - a.last.frameIndex(FrameRate.fps25),
        2);
    expect(
        c.last.frameIndex(FrameRate.fps25) - a.last.frameIndex(FrameRate.fps25),
        -3);
  });

  test('a fixed frameRate is used as given', () async {
    final rx = LtcReceiver(frameRate: FrameRate.fps25);
    feed(rx, chunks(ltcSamples(FrameRate.fps25)));
    await settle();
    expect(rx.detectedRate, FrameRate.fps25);
    await rx.close();
  });

  test('waitFor completes when the timecode passes the target', () async {
    final rx = LtcReceiver();
    final target = Timecode.parse('01:00:01:10');
    final done = rx.waitFor(target);
    feed(rx, chunks(ltcSamples(FrameRate.fps25)));
    final hit = await done;
    expect(hit, target);
    await rx.close();
  });

  test('waitFor survives detach and fails on close', () async {
    final rx = LtcReceiver();
    final pending = rx.waitFor(Timecode.parse('05:00:00:00'));
    feed(rx, chunks(ltcSamples(FrameRate.fps25)));
    await settle();
    var failed = false;
    unawaited(pending.catchError((Object e) {
      failed = e is StateError;
      return Timecode.zero;
    }));
    await rx.detach();
    await settle();
    expect(failed, isFalse);
    await rx.close();
    await settle();
    expect(failed, isTrue);
    expect(rx.waitFor(Timecode.zero), throwsStateError);
  });

  test('reports lost after silence and jump on relocation', () async {
    final a = ltcSamples(FrameRate.fps25, seconds: 2);
    final silence = Int16List(48000 ~/ 2);
    final b = ltcSamples(FrameRate.fps25,
        start: const Timecode(2, 0, 0, 0), seconds: 2);
    final all = Int16List(a.length + silence.length + b.length)
      ..setAll(0, a)
      ..setAll(a.length, silence)
      ..setAll(a.length + silence.length, b);
    final rx = LtcReceiver();
    final statuses = <StateEventType>[];
    rx.events.listen((e) => statuses.add(e.type));
    feed(rx, chunks(all));
    await settle();
    expect(statuses,
        [StateEventType.lock, StateEventType.lost, StateEventType.lock]);
    expect(rx.current!.hours, 2);
    await rx.close();

    // A relocation without a gap is a jump; the chase continues.
    final c = Int16List(a.length + b.length)
      ..setAll(0, a)
      ..setAll(a.length, b);
    final rx2 = LtcReceiver();
    final s2 = <StateEventType>[];
    rx2.events.listen((e) => s2.add(e.type));
    feed(rx2, chunks(c));
    await settle();
    expect(s2, [StateEventType.lock, StateEventType.jump]);
    expect(rx2.isLocked, isTrue);
    expect(rx2.current!.hours, 2);
    await rx2.close();
  });

  test('reads the selected channel of interleaved audio', () async {
    final mono = ltcSamples(FrameRate.fps30);
    final stereo = Int16List(mono.length * 2);
    for (var i = 0; i < mono.length; i++) {
      stereo[i * 2 + 1] = mono[i]; // channel 2 only
    }
    final rx = LtcReceiver(channel: 2);
    feed(rx, chunks(stereo, channels: 2));
    await settle();
    expect(rx.detectedRate, FrameRate.fps30);
    await rx.close();

    final rx0 = LtcReceiver(channel: 1);
    feed(rx0, chunks(stereo, channels: 2));
    await settle();
    expect(rx0.current, isNull);
    await rx0.close();
  });

  test('chunks without timestamps use the receive time', () async {
    var now = const Duration(seconds: 5);
    final rx = LtcReceiver(now: () => now);
    final got = <Timecode>[];
    rx.frames.listen(got.add);
    final c = StreamController<PcmChunk>();
    rx.attach(c.stream);
    for (final ch in chunks(ltcSamples(FrameRate.fps25))) {
      c.add(PcmChunk(samples: ch.samples, sampleRate: ch.sampleRate));
      await settle();
      now += const Duration(milliseconds: 20);
    }
    expect(got.last.timestamp, isNotNull);
    await c.close();
    await rx.close();
  });

  for (final mode in Backpressure.values) {
    test('a paused frames listener gets ${mode.name}', () async {
      final rx = LtcReceiver(backpressure: mode);
      final all = <Timecode>[];
      final slow = <Timecode>[];
      rx.frames.listen(all.add);
      final sub = rx.frames.listen(slow.add);
      await settle();
      sub.pause();
      feed(rx, chunks(ltcSamples(FrameRate.fps25)));
      await settle();
      sub.resume();
      await settle();
      expect(all.length, greaterThan(10));
      if (mode == Backpressure.latest) {
        expect(slow, [all.last]);
      } else {
        expect(slow, all);
      }
      await rx.close();
    });
  }

  test('await for with a slow body sees the newest frame', () async {
    final rx = LtcReceiver();
    final seen = <Timecode>[];
    final loop = () async {
      await for (final tc in rx.frames) {
        seen.add(tc);
        await Future<void>.delayed(const Duration(milliseconds: 5));
      }
    }();
    await settle();
    final c = feed(rx, chunks(ltcSamples(FrameRate.fps25)));
    await Future<void>.delayed(const Duration(milliseconds: 50));
    await rx.close();
    await loop;
    expect(seen.length, lessThan(10));
    expect(seen.last, rx.current);
    await c.close();
  });

  group('Timecode', () {
    test('parse and format', () {
      expect(Timecode.parse('01:02:03:04').toString(), '01:02:03:04');
      final df = Timecode.parse('01:02:03;04');
      expect(df.dropFrame, isTrue);
      expect(df.toString(), '01:02:03;04');
      expect(Timecode.tryParse('24:00:00:00'), isNull);
      expect(Timecode.tryParse('00:00:00:30'), isNull);
      expect(() => Timecode.parse('x'), throwsFormatException);
    });

    test('frame index round trip and addFrames', () {
      const rate = FrameRate.fps2997Drop;
      final tc = Timecode.parse('00:00:59;29');
      expect(tc.addFrames(1, rate).toString(), '00:01:00;02');
      expect(Timecode.fromFrameIndex(107892, rate).toString(), '01:00:00;00');
      expect(Timecode.parse('00:00:00:29').frameIndex(FrameRate.fps25), 24);
      expect(Timecode.zero.addFrames(-1, FrameRate.fps25).toString(),
          '23:59:59:24');
    });
  });

  test('end, input errors and isClosed', () async {
    final rx = LtcReceiver();
    final events = <StateEvent>[];
    final errors = <Object>[];
    rx.events.listen(events.add, onError: errors.add);
    final c = StreamController<PcmChunk>();
    rx.attach(c.stream);
    for (final chunk in chunks(ltcSamples(FrameRate.fps25))) {
      c.add(chunk);
    }
    c.addError(StateError('device unplugged'));
    await settle();
    await c.close();
    await settle();
    expect(events.map((e) => e.type),
        [StateEventType.lock, StateEventType.lost, StateEventType.end]);
    expect(events.first.timecode.toString(), '01:00:00:03');
    expect(events.first.frameRate, FrameRate.fps25);
    expect(events.first.channel, 1);
    expect(errors.single, isA<StateError>());
    expect(rx.isAttached, isFalse);
    expect(rx.isClosed, isFalse);
    await rx.close();
    expect(rx.isClosed, isTrue);
    expect(() => rx.attach(const Stream.empty()), throwsStateError);
  });
}
