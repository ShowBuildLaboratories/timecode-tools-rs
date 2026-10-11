// Receiver goldens: scripted LTC and MTC inputs run through LtcReceiver and
// MtcReceiver, with everything they report recorded per input chunk.
//
// The Rust core and the Dart and Node.js packages replay the same inputs
// from these files and must report the same frames, events, waitFor and
// framesAt results. `test/receiver_goldens_test.dart` checks that the
// committed files still match this implementation; regenerate them with
// `dart run tool/receiver_goldens.dart ../../goldens/receiver`.
import 'dart:async';
import 'dart:typed_data';

import 'package:timecode_tools_reference/timecode_tools_reference.dart';

import 'mtc_signal.dart';

/// One LTC input segment, generated with [encodeLtcSequence] (or silence).
class LtcSegment {
  const LtcSegment(this.start, this.frameCount) : silence = 0;
  const LtcSegment.silence(this.silence)
      : start = null,
        frameCount = 0;
  final String? start;
  final int frameCount;
  final int silence; // samples

  Map<String, Object?> toJson() => start == null
      ? {'silence': silence}
      : {'start': start, 'frame_count': frameCount};
}

class LtcScenario {
  const LtcScenario({
    required this.name,
    required this.description,
    required this.rate,
    required this.segments,
    this.sampleRate = 48000,
    this.chunk = 480,
    this.channels = 1,
    this.ltcChannel = 1,
    this.options = const {},
    this.waitFor = const [],
  });
  final String name;
  final String description;
  final FrameRate rate; // rate of the generated signal
  final List<LtcSegment> segments;
  final int sampleRate;
  final int chunk; // samples per channel per chunk
  final int channels;
  final int ltcChannel; // 1-based channel carrying LTC; others get a ramp
  final Map<String, Object?> options; // frame_rate, offset_frames, channel
  final List<String> waitFor;
}

class MtcScenario {
  const MtcScenario({
    required this.name,
    required this.description,
    required this.chunks,
    this.options = const {},
    this.waitFor = const [],
  });
  final String name;
  final String description;
  final List<MidiChunk> Function() chunks;
  final Map<String, Object?> options;
  final List<String> waitFor;
}

const t0Micros = 100000000; // inputs start at 100 s on the chunk clock

final ltcScenarios = <LtcScenario>[
  LtcScenario(
    name: 'ltc_25_lock_jump_loss_relock',
    description: '25 fps: lock, a relocation without a gap (jump), silence '
        '(lost), then a new timecode (lock again).',
    rate: FrameRate.fps25,
    segments: const [
      LtcSegment('01:00:00:00', 60),
      LtcSegment('02:00:00:00', 40),
      LtcSegment.silence(24000),
      LtcSegment('03:00:00:00', 40),
    ],
    waitFor: const ['01:00:01:00', '02:00:00:10', '01:00:30:00'],
  ),
  LtcScenario(
    name: 'ltc_2997df_offset_corrupt',
    description: '29.97 drop-frame across a minute boundary with one corrupt '
        'frame in place of a real one (ignored), offset +2, uneven chunks.',
    rate: FrameRate.fps2997Drop,
    segments: const [
      LtcSegment('00:00:58;00', 40),
      LtcSegment('05:00:00;00', 1),
      LtcSegment('00:00:59;11', 50),
    ],
    chunk: 777,
    options: const {'offset_frames': 2},
    waitFor: const ['00:01:00;05', '00:00:58;10'],
  ),
  LtcScenario(
    name: 'ltc_24_stereo_channel2_fixed_rate',
    description: '24 fps on channel 2 of a stereo stream (channel 1 carries a '
        'ramp), frame rate fixed, offset -3.',
    rate: FrameRate.fps24,
    segments: const [LtcSegment('10:00:00:00', 48)],
    channels: 2,
    ltcChannel: 2,
    sampleRate: 44100,
    chunk: 441,
    options: const {'frame_rate': '24', 'channel': 2, 'offset_frames': -3},
    waitFor: const ['10:00:01:00'],
  ),
  LtcScenario(
    name: 'ltc_30_midnight',
    description: '30 fps non-drop through midnight; waitFor just after it.',
    rate: FrameRate.fps30,
    segments: const [LtcSegment('23:59:59:00', 60)],
    chunk: 1024,
    waitFor: const ['00:00:00:05'],
  ),
];

final mtcScenarios = <MtcScenario>[
  MtcScenario(
    name: 'mtc_25_lock_locate_relock',
    description: '25 fps quarter frames, a full-frame locate (lost, jump), '
        'then quarter frames from the new position (lock again).',
    chunks: () => [
      ...quarterFrames(Timecode.zero, FrameRate.fps25, 40,
          t0: const Duration(microseconds: t0Micros)),
      MidiChunk(fullFrame(Timecode.parse('01:00:00:00'), FrameRate.fps25),
          timestamp: const Duration(microseconds: t0Micros + 2000000)),
      ...quarterFrames(Timecode.parse('01:00:00:00'), FrameRate.fps25, 40,
          t0: const Duration(microseconds: t0Micros + 3000000)),
    ],
    waitFor: const ['00:00:01:00', '01:00:00:20'],
  ),
  MtcScenario(
    name: 'mtc_2997df_relocate_noise_offset',
    description: '29.97 drop-frame quarter frames with MIDI clock bytes in '
        'between, a relocation without a full frame (jump), offset -1.',
    chunks: () => [
      ...quarterFrames(Timecode.parse('00:00:58;00'), FrameRate.fps2997Drop, 60,
          t0: const Duration(microseconds: t0Micros), interleaveClock: true),
      ...quarterFrames(Timecode.parse('10:00:00;00'), FrameRate.fps2997Drop, 30,
          t0: Duration(
              microseconds: t0Micros + (60e6 / (30000 / 1001)).round()),
          interleaveClock: true),
    ],
    options: const {'offset_frames': -1},
    waitFor: const ['00:00:59;20', '10:00:00;10'],
  ),
  MtcScenario(
    name: 'mtc_30_stop',
    description: '30 fps quarter frames, then only MIDI clock: lost after '
        'lostAfter (300 ms).',
    chunks: () => [
      ...quarterFrames(Timecode.parse('12:00:00:00'), FrameRate.fps30, 30,
          t0: const Duration(microseconds: t0Micros)),
      for (var i = 1; i <= 10; i++)
        MidiChunk(Uint8List.fromList([0xF8]),
            timestamp: Duration(microseconds: t0Micros + 1000000 + i * 50000)),
    ],
  ),
];

/// LTC samples (interleaved when [LtcScenario.channels] > 1).
Int16List ltcInput(LtcScenario s) {
  final mono = <int>[];
  for (final seg in s.segments) {
    if (seg.start == null) {
      mono.addAll(List<int>.filled(seg.silence, 0));
      continue;
    }
    final tc = Timecode.parse(seg.start!);
    final bytes = encodeLtcSequence(
      sampleRate: s.sampleRate,
      fps: s.rate.nominal,
      dropFrame: s.rate.dropFrame,
      startHours: tc.hours,
      startMinutes: tc.minutes,
      startSeconds: tc.seconds,
      startFrames: tc.frames,
      frameCount: seg.frameCount,
    );
    mono.addAll(Int16List.view(bytes.buffer, 0, bytes.length ~/ 2));
  }
  if (s.channels == 1) return Int16List.fromList(mono);
  final out = Int16List(mono.length * s.channels);
  for (var i = 0; i < mono.length; i++) {
    for (var c = 0; c < s.channels; c++) {
      out[i * s.channels + c] = c + 1 == s.ltcChannel ? mono[i] : ramp(i);
    }
  }
  return out;
}

/// The filler on non-LTC channels: a 1 kHz-ish sawtooth, easy to reproduce.
int ramp(int i) => (i * 37) % 4000 - 2000;

Map<String, Object?> _tc(Timecode tc) => {
      'tc': tc.toString(),
      'ts_us': tc.timestamp?.inMicroseconds,
    };

Future<Map<String, Object?>> _run<T>(
  TimecodeReceiver<T> rx,
  List<T> input,
  List<String> targets,
  Duration Function(T chunk) queryAt,
) async {
  final steps = <Map<String, Object?>>[];
  final frames = <Map<String, Object?>>[];
  final events = <String>[];
  final resolved = <String, Object?>{};
  rx.frames.listen((tc) => frames.add(_tc(tc)));
  rx.events.listen((e) => events.add(e.type.name));
  for (final t in targets) {
    resolved[t] = null;
    unawaited(rx.waitFor(Timecode.parse(t)).then(
          (tc) => resolved[t] = {'chunk': steps.length, ..._tc(tc)},
          onError: (_) {},
        ));
  }
  final c = StreamController<T>();
  rx.attach(c.stream);
  for (final chunk in input) {
    c.add(chunk);
    await Future<void>.delayed(Duration.zero);
    // framesAt at the end of the chunk (LTC) or when it arrived (MTC).
    final at = queryAt(chunk);
    steps.add({
      'frames': List.of(frames),
      'events': List.of(events),
      'frames_at': {'t_us': at.inMicroseconds, 'value': rx.framesAt(at)},
    });
    frames.clear();
    events.clear();
  }
  final current = rx.current;
  final result = {
    'steps': steps,
    'wait_for': resolved,
    'final_locked': rx.isLocked,
    'final_detected_rate': rx.detectedRate?.label,
    'final_current': current?.toString(),
  };
  await rx.close();
  await c.close();
  return result;
}

String _rateName(FrameRate r) => switch (r) {
      FrameRate.fps2997Drop => '29.97df',
      _ => '${r.nominal}',
    };

FrameRate _rateFromName(Object? name) => switch (name) {
      '24' => FrameRate.fps24,
      '25' => FrameRate.fps25,
      '29.97df' => FrameRate.fps2997Drop,
      '30' => FrameRate.fps30,
      _ => FrameRate.auto,
    };

Future<Map<String, Object?>> ltcGolden(LtcScenario s) async {
  final samples = ltcInput(s);
  final per = s.chunk * s.channels;
  final input = <PcmChunk>[
    for (var i = 0; i < samples.length; i += per)
      PcmChunk(
        samples: Int16List.sublistView(
            samples, i, i + per < samples.length ? i + per : samples.length),
        sampleRate: s.sampleRate,
        channels: s.channels,
        timestamp: Duration(
            microseconds:
                t0Micros + (i ~/ s.channels * 1e6 / s.sampleRate).round()),
      ),
  ];
  final rx = LtcReceiver(
    frameRate: _rateFromName(s.options['frame_rate']),
    offsetFrames: (s.options['offset_frames'] as int?) ?? 0,
    channel: (s.options['channel'] as int?) ?? 1,
  );
  return {
    'name': s.name,
    'description': s.description,
    'kind': 'ltc',
    'input': {
      'sample_rate': s.sampleRate,
      'rate': _rateName(s.rate),
      'channels': s.channels,
      'ltc_channel': s.ltcChannel,
      'chunk': s.chunk,
      't0_us': t0Micros,
      'segments': [for (final seg in s.segments) seg.toJson()],
      'sample_count': samples.length,
    },
    'options': s.options,
    ...await _run(
      rx,
      input,
      s.waitFor,
      (c) =>
          c.timestamp! +
          Duration(
              microseconds:
                  (c.samples.length ~/ c.channels * 1e6 / c.sampleRate)
                      .round()),
    ),
  };
}

Future<Map<String, Object?>> mtcGolden(MtcScenario s) async {
  final input = s.chunks();
  final rx = MtcReceiver(
    frameRate: _rateFromName(s.options['frame_rate']),
    offsetFrames: (s.options['offset_frames'] as int?) ?? 0,
  );
  return {
    'name': s.name,
    'description': s.description,
    'kind': 'mtc',
    'input': {
      'chunks': [
        for (final c in input)
          {
            'bytes': [
              for (final b in c.bytes) b.toRadixString(16).padLeft(2, '0')
            ].join(),
            't_us': c.timestamp!.inMicroseconds,
          },
      ],
    },
    'options': s.options,
    ...await _run(rx, input, s.waitFor, (c) => c.timestamp!),
  };
}

Future<Map<String, Map<String, Object?>>> allGoldens() async => {
      for (final s in ltcScenarios) s.name: await ltcGolden(s),
      for (final s in mtcScenarios) s.name: await mtcGolden(s),
    };
