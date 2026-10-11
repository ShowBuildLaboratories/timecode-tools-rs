import 'dart:typed_data';

/// One chunk of captured audio for `LtcReceiver.attach`.
class PcmChunk {
  /// [samples] are signed 16-bit PCM, interleaved when [channels] > 1.
  ///
  /// [timestamp] is when the chunk's *first* sample was captured, on a
  /// monotonic clock (one that never jumps, such as a `Stopwatch` or the
  /// platform's host time). Leave it `null` if the capture API does not
  /// provide one: the receiver then uses the time it received the chunk,
  /// which adds the delivery jitter and latency to every timestamp.
  const PcmChunk({
    required this.samples,
    required this.sampleRate,
    this.channels = 1,
    this.timestamp,
  }) : assert(channels >= 1);

  /// Converts float samples (-1.0..1.0) to PCM16.
  factory PcmChunk.fromFloat32(
    Float32List samples, {
    required int sampleRate,
    int channels = 1,
    Duration? timestamp,
  }) {
    final out = Int16List(samples.length);
    for (var i = 0; i < samples.length; i++) {
      final v = (samples[i] * 32767).round();
      out[i] = v > 32767 ? 32767 : (v < -32768 ? -32768 : v);
    }
    return PcmChunk(
      samples: out,
      sampleRate: sampleRate,
      channels: channels,
      timestamp: timestamp,
    );
  }

  final Int16List samples;
  final int sampleRate;
  final int channels;
  final Duration? timestamp;

  /// Samples per channel.
  int get frameCount => samples.length ~/ channels;
}
