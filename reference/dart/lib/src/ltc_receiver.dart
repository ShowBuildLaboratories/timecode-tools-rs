part of 'receiver.dart';

/// Reads LTC from a stream of PCM audio.
///
/// ```dart
/// final rx = LtcReceiver(frameRate: FrameRate.auto, offsetFrames: 0);
/// rx.attach(pcmStream);
/// rx.frames.listen((tc) => print(tc));
/// rx.events.listen((e) => print(e));
/// await rx.waitFor(Timecode.parse('01:00:10:00'));
/// await rx.detach();
/// ```
///
/// The receiver does no audio capture itself; the app supplies
/// [PcmChunk]s from whatever capture API it uses.
class LtcReceiver extends _ReceiverBase<PcmChunk> {
  LtcReceiver({
    super.frameRate = FrameRate.auto,
    super.offsetFrames = 0,
    this.channel = 1,
    super.lostAfter = const Duration(milliseconds: 300),
    super.backpressure = Backpressure.latest,
    super.now,
  });

  /// Which channel (1-based) of multi-channel chunks carries LTC.
  int channel;

  @override
  int? get _channel => channel;

  LtcDecoder? _decoder;
  int _sampleRate = 0;
  int _channels = 0;
  double _level = 0;

  /// Recent peak input level of the LTC channel, 0..1.
  double get level => _level;

  @override
  void _onChunk(PcmChunk chunk) {
    final at = chunk.timestamp ?? _now();
    if (_decoder == null ||
        chunk.sampleRate != _sampleRate ||
        chunk.channels != _channels) {
      // A new input format: decoding starts over.
      _lost();
      _decoder = LtcDecoder(sampleRate: chunk.sampleRate);
      _sampleRate = chunk.sampleRate;
      _channels = chunk.channels;
      _clock.reset();
    }
    final decoder = _decoder!;
    final sr = chunk.sampleRate.toDouble();
    if (channel < 1 || channel > chunk.channels) return;
    final mono = chunk.channels == 1
        ? chunk.samples
        : _deinterleave(chunk.samples, chunk.channels, channel - 1);

    final startSample = decoder.sampleIndex;
    _clock.noteHostTime(startSample / sr, at.inMicroseconds);
    final decoded = decoder.push(mono);
    _level = decoder.signalLevel;

    for (final f in decoded) {
      final rate = frameRate != FrameRate.auto
          ? frameRate
          : FrameRate.fromDecoded(fps: f.estimatedFps, dropFrame: f.dropFrame);
      if (rate == null) continue;
      final index =
          Timecode(f.hours, f.minutes, f.seconds, f.frames).frameIndex(rate);
      // The sync word ends where the next frame begins; this frame began one
      // frame period earlier.
      final endAt = at +
          Duration(
            microseconds: ((f.sampleIndex - startSample) * 1e6 / sr).round(),
          );
      final startAt = endAt - Duration(microseconds: (1e6 / rate.fps).round());
      _clock.addObservation(
        f.sampleIndex / sr,
        (index + 1).toDouble(),
        framesPerDay: rate.framesPerDay,
      );
      // Continuity and loss are judged on sample time, which has no
      // delivery jitter.
      _onReading(
        Timecode.fromFrameIndex(index + offsetFrames, rate, timestamp: startAt),
        f.sampleIndex / sr,
        sourceLocked: f.locked,
      );
    }
    _checkLoss(decoder.sampleIndex / sr);
  }

  static Int16List _deinterleave(Int16List s, int channels, int ch) {
    final n = s.length ~/ channels;
    final out = Int16List(n);
    for (var i = 0; i < n; i++) {
      out[i] = s[i * channels + ch];
    }
    return out;
  }
}
