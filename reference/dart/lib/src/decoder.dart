/// A pure-Dart, plugin-free streaming SMPTE LTC (linear/longitudinal
/// timecode) decoder.
///
/// This file has no dependency on `dart:io`, Flutter, or any platform
/// channel — it only operates on raw PCM16 little-endian mono samples
/// handed to it via [LtcDecoder.push] / [LtcDecoder.pushBytes]. That keeps it fully unit-testable
/// with synthetic audio and portable to any platform that can capture
/// audio.
///
/// ## Algorithm
///
/// LTC encodes 80 bits per frame using biphase mark code (BMC): a signal
/// transition always occurs at the start of every bit cell; an *additional*
/// transition occurs at the midpoint of the cell if, and only if, the bit is
/// `1`. Decoding therefore never looks at signal polarity (rising vs.
/// falling) — only at the *spacing* between zero crossings — which makes the
/// decoder inherently polarity-agnostic: inverting the whole signal leaves
/// the crossing timings, and therefore the decoded bits, unchanged.
///
/// The decoder tracks the sample distance between successive zero
/// crossings and keeps an adaptive estimate of the full bit-cell period.
/// Each interval shorter than ~0.75x that estimate is a "half cell"; a pair
/// of consecutive half cells decodes to bit `1`. An interval at
/// approximately the full period decodes to bit `0`. Decoded bits are fed
/// into an 80-bit sliding window; once the most recently arrived 16 bits
/// equal the LTC sync word (`0011111111111101`, matched in arrival order),
/// the preceding 64 bits are parsed as one SMPTE 12M timecode frame.
///
/// ## Bit-cell period bootstrap
///
/// Classifying an interval as a half cell or a full cell requires an
/// estimate of the full period first — but the very first interval measured
/// could equally be either kind, depending purely on where in the bit
/// stream capture happened to start. Guessing wrong on that first interval
/// is not self-correcting: [_updatePeriod]'s exponential smoothing pulls the
/// estimate *toward* whatever `fullSpan` each classification decision
/// produces, and a wrong initial period biases every subsequent
/// classification the same wrong way — an interval genuinely 1x the true
/// period reads as "half" against a period estimate that is 2x too large,
/// and gets paired with the next one into a `fullSpan` that reinforces the
/// same wrong 2x period. That is a stable harmonic mislock, not a transient
/// error, and it would never self-correct without new evidence.
///
/// So the decoder does not decide a period from a single interval. It
/// buffers the first several measured intervals (see
/// `_bootstrapIntervals`), then looks at both hypotheses at once: the
/// smallest observed interval can only be a half cell or smaller (biphase
/// mark code has no interval shorter than a half cell), so doubling it is
/// one candidate period; the largest observed interval, if clearly larger
/// (roughly 1.5-2.5x the smallest), is itself a full-cell candidate. When
/// both a small and a large cluster are present in the buffered window,
/// their two independent period estimates are averaged for the initial
/// guess — correct regardless of whether capture happened to start mid
/// half-cell or mid full-cell. If the buffered window has not yet shown two
/// distinct clusters (still ambiguous — e.g. a long run of same-length
/// cells), buffering continues, up to a bounded cap, since a real LTC frame
/// always mixes 0 and 1 bits within a short span; past the cap, the
/// single-cluster case falls back to doubling the smallest interval, same
/// as before. Once a period is decided, every buffered interval is replayed
/// through the normal half/full classification so no real bits are lost to
/// the bootstrap delay.
library;

import 'dart:math' as math;
import 'dart:typed_data';

import 'timecode.dart';

/// One decoded SMPTE LTC frame.
class LtcFrame {
  const LtcFrame({
    required this.hours,
    required this.minutes,
    required this.seconds,
    required this.frames,
    required this.dropFrame,
    this.sampleIndex = 0,
    this.locked = false,
    this.estimatedFps,
  });

  final int hours;
  final int minutes;
  final int seconds;
  final int frames;
  final bool dropFrame;

  /// Cumulative count of PCM samples fed to the decoder (across every
  /// [LtcDecoder.push] call since the decoder was constructed) at the point
  /// this frame's sync word finished. This is the decoder's own sample
  /// clock — it advances only with real audio samples, so it is immune to
  /// host callback/chunk-delivery jitter. [LtcClockModel] divides
  /// this by the sample rate to get the regression's time axis; see its
  /// docs for why that matters.
  ///
  /// Deliberately excluded from [==]/[hashCode] so tests (and any other
  /// code) that compare frames by decoded timecode value alone are
  /// unaffected by exactly which sample the sync word happened to land on.
  final int sampleIndex;

  /// [LtcDecoder.locked] right after this frame was decoded. Not part of
  /// [==]/[hashCode].
  final bool locked;

  /// [LtcDecoder.estimatedFps] right after this frame was decoded. Not part
  /// of [==]/[hashCode].
  final int? estimatedFps;

  /// `hh:mm:ss:ff` for non-drop-frame timecode, `hh:mm:ss;ff` for
  /// drop-frame, matching standard SMPTE display convention.
  String get formatted {
    String two(int value) => value.toString().padLeft(2, '0');
    final frameSeparator = dropFrame ? ';' : ':';
    return '${two(hours)}:${two(minutes)}:${two(seconds)}'
        '$frameSeparator${two(frames)}';
  }

  @override
  String toString() => formatted;

  @override
  bool operator ==(Object other) =>
      other is LtcFrame &&
      other.hours == hours &&
      other.minutes == minutes &&
      other.seconds == seconds &&
      other.frames == frames &&
      other.dropFrame == dropFrame;

  @override
  int get hashCode => Object.hash(hours, minutes, seconds, frames, dropFrame);
}

/// Streaming biphase-mark SMPTE LTC decoder (the low-level interface).
///
/// Push mono PCM16 samples with [push] (or little-endian bytes with
/// [pushBytes]); each call returns the frames whose sync word completed
/// inside that chunk. State (partial samples across a byte boundary,
/// pending half-cell, bit shift register, lock state) is carried between
/// calls, so chunks may be arbitrarily sized and unevenly split.
///
/// For a higher-level interface with frame-rate selection, offset, status
/// and a smoothed clock, see `LtcReceiver`.
class LtcDecoder {
  LtcDecoder({required this.sampleRate});

  /// The sample rate of the PCM audio passed to [push], in Hz.
  final int sampleRate;

  /// The exact 16-bit LTC sync word, in arrival (transmission) order.
  static const String syncWordBits = '0011111111111101';

  // Frames completed during the current push call.
  final List<LtcFrame> _out = <LtcFrame>[];

  // --- PCM byte carry across pushBytes() calls ------------------------
  int? _carryByte;

  // --- Zero-crossing / Schmitt-trigger edge detection ------------------
  static const int _minThreshold = 400; // absolute floor, ~1.2% of full scale
  static const double _peakDecay = 0.999;
  double _peak = 0;
  bool? _polarityPositive;
  int _sampleIndex = 0;
  int _lastEdgeIndex = 0;
  bool _haveEdgeReference = false;

  // --- Biphase-mark bit classification ---------------------------------
  double? _periodEstimate; // samples per full bit cell
  bool _pendingHalf = false;
  double _pendingHalfInterval = 0;

  // --- Period bootstrap (see this file's top-level "Bit-cell period
  // bootstrap" doc) ------------------------------------------------------
  final List<int> _bootstrapIntervals = <int>[];
  static const int _bootstrapMinSamples = 6;
  static const int _bootstrapMaxSamples = 32;

  /// Cumulative sample index at which the most recently emitted bit's
  /// closing edge was observed. See [LtcFrame.sampleIndex].
  int get sampleIndex => _sampleIndex;

  // --- 80-bit sliding shift register (oldest bit at index 0) ----------
  final List<int> _bits = <int>[];
  static final List<int> _syncPattern =
      syncWordBits.split('').map(int.parse).toList(growable: false);

  // --- Lock / contiguity tracking --------------------------------------
  LtcFrame? _lastFrame;
  int _lastFrameFpsGuess = 30;
  int _consecutiveContiguous = 0;
  int? _fps;
  int _observedMaxFrames = -1;

  /// `true` once at least 4 consecutive frames have been decoded with
  /// contiguous timecode (each frame exactly one tick after the last).
  bool get locked => _consecutiveContiguous >= 4;

  /// Recent peak `|sample|`, normalized to 0..1 against full scale.
  double get signalLevel => (_peak / 32768).clamp(0, 1);

  /// Estimated frame rate (24, 25 or 30) derived from the observed bit rate
  /// and cross-checked against the largest frame-number field seen so far.
  /// `null` until enough locked signal has been seen to know — this is
  /// never guessed or defaulted.
  int? get estimatedFps => _fps;

  /// The drop-frame flag of the most recently decoded frame; `null` before
  /// any frame has decoded. Real decoded state only — never assumed.
  bool? get lastDropFrame => _lastFrame?.dropFrame;

  /// Decodes the next chunk of mono PCM16 samples and returns the frames
  /// completed in it (empty for silence, noise, or a partial frame).
  List<LtcFrame> push(Int16List samples) {
    _out.clear();
    for (var i = 0; i < samples.length; i++) {
      _processSample(samples[i]);
    }
    return List<LtcFrame>.of(_out);
  }

  /// Like [push], for raw PCM16 little-endian mono bytes. An odd trailing
  /// byte is carried over to the next call.
  List<LtcFrame> pushBytes(Uint8List pcm16) {
    _out.clear();
    _feed(pcm16);
    return List<LtcFrame>.of(_out);
  }

  void _feed(Uint8List pcm16) {
    var index = 0;
    final length = pcm16.length;
    if (_carryByte != null) {
      if (length == 0) return;
      _processSample(_toInt16(_carryByte!, pcm16[0]));
      _carryByte = null;
      index = 1;
    }
    final remaining = length - index;
    final pairCount = remaining ~/ 2;
    for (var i = 0; i < pairCount; i++) {
      final base = index + i * 2;
      _processSample(_toInt16(pcm16[base], pcm16[base + 1]));
    }
    final consumed = index + pairCount * 2;
    if (consumed < length) {
      _carryByte = pcm16[length - 1];
    }
  }

  static int _toInt16(int lo, int hi) {
    final value = (hi << 8) | lo;
    return value >= 0x8000 ? value - 0x10000 : value;
  }

  void _processSample(int sample) {
    _sampleIndex++;

    final absSample = sample.abs().toDouble();
    _peak = absSample > _peak ? absSample : _peak * _peakDecay;

    final threshold = math.max(_minThreshold, _peak * 0.1);
    final wasPositive = _polarityPositive;
    if (sample > threshold) {
      _polarityPositive = true;
    } else if (sample < -threshold) {
      _polarityPositive = false;
    }
    // Inside the deadband: hold the previous polarity (Schmitt trigger).

    if (_polarityPositive == null) return;

    if (!_haveEdgeReference) {
      // First time polarity resolves: start measuring intervals from here,
      // don't treat it as an edge yet.
      _haveEdgeReference = true;
      _lastEdgeIndex = _sampleIndex;
      return;
    }

    if (wasPositive != null && wasPositive != _polarityPositive) {
      final interval = _sampleIndex - _lastEdgeIndex;
      _lastEdgeIndex = _sampleIndex;
      if (interval > 0) {
        _onEdge(interval);
      }
    }
  }

  void _onEdge(int intervalSamples) {
    if (_periodEstimate == null) {
      _bootstrapIntervals.add(intervalSamples);
      if (_bootstrapIntervals.length < _bootstrapMinSamples) return;

      final minInterval = _bootstrapIntervals.reduce(math.min);
      final maxInterval = _bootstrapIntervals.reduce(math.max);
      final haveBothClusters = maxInterval >= minInterval * 1.5;
      if (!haveBothClusters &&
          _bootstrapIntervals.length < _bootstrapMaxSamples) {
        // Still ambiguous (every buffered interval is roughly the same
        // length) — keep buffering rather than guess. See the top-level
        // "Bit-cell period bootstrap" doc.
        return;
      }

      _periodEstimate = haveBothClusters
          // Both a half-cell-sized and a full-cell-sized cluster are
          // present: average the two independent period estimates they
          // each imply, correct regardless of which kind the very first
          // buffered interval happened to be.
          ? (minInterval * 2.0 + maxInterval) / 2.0
          // Single cluster even after the extended window (e.g. an
          // unusually long run of same-length cells): a true half-cell
          // interval can never be smaller than the smallest one observed,
          // so doubling it is the best available hypothesis. Wrong only in
          // the (very rare in real LTC content, which always mixes 0 and 1
          // bits within a frame) case where the whole buffered window was a
          // run of full cells with no half cell at all.
          : minInterval * 2.0;

      final buffered = List<int>.of(_bootstrapIntervals);
      _bootstrapIntervals.clear();
      for (final sample in buffered) {
        _classifyInterval(sample);
      }
      return;
    }

    _classifyInterval(intervalSamples);
  }

  void _classifyInterval(int intervalSamples) {
    final threshold = _periodEstimate! * 0.75;
    final isHalf = intervalSamples < threshold;

    if (isHalf) {
      if (_pendingHalf) {
        final fullSpan = _pendingHalfInterval + intervalSamples;
        _updatePeriod(fullSpan);
        _emitBit(1);
        _pendingHalf = false;
      } else {
        _pendingHalf = true;
        _pendingHalfInterval = intervalSamples.toDouble();
      }
    } else {
      // A full-cell interval always starts a fresh cell boundary; a stale
      // pending half (desync/noise) is dropped rather than mis-paired.
      _pendingHalf = false;
      _updatePeriod(intervalSamples.toDouble());
      _emitBit(0);
    }
  }

  void _updatePeriod(double span) {
    final estimate = _periodEstimate;
    _periodEstimate =
        estimate == null ? span : (estimate * 0.85) + (span * 0.15);
  }

  void _emitBit(int bit) {
    _bits.add(bit);
    if (_bits.length > 80) {
      _bits.removeAt(0);
    }
    if (_bits.length == 80 && _lastSixteenMatchSync()) {
      _tryDecodeFrame();
    }
  }

  bool _lastSixteenMatchSync() {
    final offset = _bits.length - 16;
    for (var i = 0; i < 16; i++) {
      if (_bits[offset + i] != _syncPattern[i]) return false;
    }
    return true;
  }

  static int _bcd(List<int> bits, int start, int count) {
    var value = 0;
    for (var k = 0; k < count; k++) {
      value |= bits[start + k] << k;
    }
    return value;
  }

  void _tryDecodeFrame() {
    final payload = _bits.sublist(0, 64);

    final frameUnits = _bcd(payload, 0, 4);
    final frameTens = _bcd(payload, 8, 2);
    final dropFrame = payload[10] == 1;
    final secondsUnits = _bcd(payload, 16, 4);
    final secondsTens = _bcd(payload, 24, 3);
    final minutesUnits = _bcd(payload, 32, 4);
    final minutesTens = _bcd(payload, 40, 3);
    final hoursUnits = _bcd(payload, 48, 4);
    final hoursTens = _bcd(payload, 56, 2);

    final frameNum = frameTens * 10 + frameUnits;
    final seconds = secondsTens * 10 + secondsUnits;
    final minutes = minutesTens * 10 + minutesUnits;
    final hours = hoursTens * 10 + hoursUnits;

    // Sanity gate: a real LTC frame's BCD fields always fall in-range. This
    // rejects the astronomically rare accidental sync-word match inside
    // noise (~1/65536 per bit position) before it can masquerade as a
    // decoded frame.
    if (frameNum > 39 || seconds > 59 || minutes > 59 || hours > 29) {
      return;
    }

    final frame = LtcFrame(
      hours: hours,
      minutes: minutes,
      seconds: seconds,
      frames: frameNum,
      dropFrame: dropFrame,
      // The edge that just triggered this decode is the sync word's final
      // (16th) bit closing edge, so `_sampleIndex` here is exactly the
      // sample at which the sync word completed.
      sampleIndex: _sampleIndex,
    );

    _observedMaxFrames = math.max(_observedMaxFrames, frameNum);
    _updateFpsEstimate();

    final fpsGuess = _rawBitrateFpsGuess();
    final total = _totalFrameCount(frame, fpsGuess);
    final previous = _lastFrame;
    if (previous != null) {
      final previousTotal = _totalFrameCount(previous, _lastFrameFpsGuess);
      final isNextTick = total == previousTotal + 1;
      // A same-day midnight rollover (23:59:59:<last> -> 00:00:00:00) is a
      // legitimate one-tick advance too, not a discontinuity — without this
      // check it would look like a huge backward jump and drop lock right
      // at midnight.
      final framesPerDayPrev = ltcFramesPerDay(
          fps: _lastFrameFpsGuess, dropFrame: previous.dropFrame);
      final isMidnightWrap =
          total == 0 && previousTotal == framesPerDayPrev - 1;
      _consecutiveContiguous =
          (isNextTick || isMidnightWrap) ? _consecutiveContiguous + 1 : 1;
    } else {
      _consecutiveContiguous = 1;
    }
    _lastFrame = frame;
    _lastFrameFpsGuess = fpsGuess;

    _out.add(LtcFrame(
      hours: frame.hours,
      minutes: frame.minutes,
      seconds: frame.seconds,
      frames: frame.frames,
      dropFrame: frame.dropFrame,
      sampleIndex: frame.sampleIndex,
      locked: locked,
      estimatedFps: _fps,
    ));
  }

  /// Same-day, zero-based frame index for [frame] — see [ltcFrameIndex] in
  /// `timecode.dart`. [fpsGuess] is the nominal encoding rate (24/25/30)
  /// used for both non-drop-frame math and (per SMPTE) drop-frame's own
  /// frame-count field width. Drop-frame-aware: naive `seconds*fps+frames`
  /// arithmetic would treat drop-frame's skipped frame numbers (…;29 ->
  /// …;02 at the start of every non-tenth minute) as a +3 jump and break
  /// [locked]'s contiguity check once a minute for any drop-frame source.
  static int _totalFrameCount(LtcFrame frame, int fpsGuess) => ltcFrameIndex(
        hours: frame.hours,
        minutes: frame.minutes,
        seconds: frame.seconds,
        frames: frame.frames,
        dropFrame: frame.dropFrame,
        fps: fpsGuess,
      );

  int _rawBitrateFpsGuess() {
    final period = _periodEstimate;
    if (period == null || period <= 0) return 30;
    final bitsPerSecond = sampleRate / period;
    final fps = bitsPerSecond / 80;
    const candidates = [24, 25, 30];
    var best = candidates.first;
    var bestDelta = (fps - best).abs();
    for (final candidate in candidates.skip(1)) {
      final delta = (fps - candidate).abs();
      if (delta < bestDelta) {
        best = candidate;
        bestDelta = delta;
      }
    }
    return best;
  }

  void _updateFpsEstimate() {
    final period = _periodEstimate;
    if (period == null || period <= 0) return;
    final bitsPerSecond = sampleRate / period;
    final fps = bitsPerSecond / 80;
    int? matched;
    if ((fps - 24).abs() < 0.75) {
      matched = 24;
    } else if ((fps - 25).abs() < 0.75) {
      matched = 25;
    } else if ((fps - 30).abs() < 0.9) {
      matched = 30;
    }
    if (matched != null && matched > _observedMaxFrames) {
      _fps = matched;
    }
  }
}
