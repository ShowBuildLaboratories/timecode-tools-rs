/// A pure-Dart, plugin-free regression clock for decoded LTC.
///
/// This file has no dependency on `dart:io`, Flutter, or any plugin — same
/// portability goal as the decoder.
///
/// ## Why regression, not "last frame + elapsed wall time"
///
/// A naive LTC reader tracks "the last decoded frame, plus wall-clock time
/// elapsed since it arrived". That is fine for a cosmetic readout, but
/// unusable for chasing or cue triggers: audio capture delivers PCM in
/// ~100 ms host callback chunks, so decoded frames arrive in bursts with
/// irregular host-clock timestamps — jitter that has nothing to do with the
/// real timecode rate.
///
/// [LtcClockModel] instead regresses timecode frame count against the
/// *capture sample clock* — the cumulative count of PCM samples the decoder
/// has consumed, divided by the sample rate (see [LtcFrame.sampleIndex]).
/// The sample clock is the audio device's own clock:
/// it advances by exactly one tick per sample regardless of when or in what
/// size chunks the host delivers callbacks, so it is immune to host
/// scheduling jitter. Fitting `tcFrames(t) = a + b*t` by least squares over
/// a short sliding window of (sampleTime, tcFrameCount) observations gives:
///
/// - `a + b*t`: a smooth, interpolated timecode prediction at any sample
///   time, including sub-frame — [tcAtSampleTime].
/// - `b`: the *real* frame rate the source is actually running at, in real
///   frames per real second, not the nominal rate encoded in the bits. This
///   is what distinguishes 23.976 fps content (which still encodes a 24 in
///   its BCD fields) from true 24.0, and what detects varispeed (a source
///   drifting a fraction of a percent off nominal) — see
///   [measuredFrameRate].
///
/// ## Discontinuities are jams, not blends
///
/// If a new observation's decoded timecode lands far from what the current
/// fit predicts (more than [jumpResetFrames]), that is not measurement
/// noise — it means the operator relocated the transport or switched
/// sources. The model resets its window and starts fitting a fresh line
/// from that point, rather than blending the old and new timelines into a
/// meaningless average slope. A same-day-boundary wraparound (23:59:59:xx
/// rolling to 00:00:00:00) is the one large jump that is *not* a
/// discontinuity; see [addObservation]'s `framesPerDay` parameter.
///
/// ## Host-time mapping is separate, and lossy on purpose
///
/// [noteHostTime] / [tcAtHostMicros] maintain a second, much simpler
/// mapping from host time (see "Monotonic timing" below) at chunk-arrival
/// time to the same sample-clock time axis, so callers that need to
/// schedule something against a host `Timer` (chasing, cue triggers) have
/// a way in. This estimate necessarily bakes in capture latency (audio
/// hardware buffering plus roughly one host delivery chunk, typically tens
/// of milliseconds) between "the sample was actually captured" and "the
/// host process observed it arrive." This model does not attempt to
/// measure or remove that latency; compensate for it in the caller (for
/// example with a user-adjustable offset), the way broadcast LTC readers
/// usually do.
///
/// ## Monotonic timing
///
/// Both the host-time mapping above and this model's own staleness check
/// (see [usable], [_stale]) deliberately avoid
/// `DateTime.now().microsecondsSinceEpoch` / [DateTime.now]. That is wall
/// clock time — a system NTP step or manual clock change between two
/// timestamps is indistinguishable, from this model's point of view, from
/// real elapsed time. A backward step can make a stale source look fresh
/// (or an already-fresh source look impossibly stale, if `_stale`'s
/// subtraction goes negative and duration comparisons behave surprisingly);
/// worse, [tcAtHostMicros] extrapolates the regression fit *forward* by
/// whatever `hostMicros - offset` implies, so a forward wall-clock jump can
/// make it report a timecode far in the future, which a naive cue-trigger
/// caller could read as "we already passed the trigger point" and fire (or
/// skip) a cue that was never actually reached.
///
/// Everything internal to this class is therefore timestamped from
/// [monotonicMicrosNow] — backed by a single process-lifetime [Stopwatch],
/// which cannot jump — instead of [DateTime]. [noteHostTime]'s `hostMicros`
/// and [tcAtHostMicros]'s `hostMicros` parameter are consequently **not**
/// `DateTime.now().microsecondsSinceEpoch`; every caller (typically the
/// code that receives audio chunks from the capture device) must source
/// that value from [monotonicMicrosNow] too, so both sides of the mapping share the
/// same epoch. [DateTime] remains the right choice for anything actually
/// *displayed* to the operator — this file just never uses it to reason
/// about elapsed time.
library;

import 'dart:collection';
import 'dart:math' as math;

import 'timecode.dart';

/// A single [Stopwatch], started the first time this library is loaded,
/// backing every monotonic timestamp in this file (and in the caller's
/// host-time reporting). See this file's
/// top-level "Monotonic timing" doc for why a [Stopwatch] is used instead of
/// [DateTime.now]: it cannot be stepped backward or forward by an NTP
/// correction or a manual clock change, so elapsed-time math against it
/// stays correct across one.
///
/// The returned values are only meaningful as a *difference* between two
/// calls within this process's lifetime — never compare them to
/// [DateTime.now().microsecondsSinceEpoch] or to another process's clock.
final Stopwatch _processMonotonicClock = Stopwatch()..start();

/// Monotonic microseconds elapsed since this library was first loaded
/// (effectively, the process's start). See the [_processMonotonicClock]
/// doc for why this exists instead of `DateTime.now().microsecondsSinceEpoch`.
int monotonicMicrosNow() => _processMonotonicClock.elapsedMicroseconds;

/// [monotonicMicrosNow], as a [Duration] rather than a raw `int` of
/// microseconds. [LtcClockModel]'s default staleness clock — see
/// `elapsedSinceStart` on its constructor.
Duration monotonicMicrosNowAsDuration() => _processMonotonicClock.elapsed;

class _Observation {
  const _Observation(this.t, this.tc);
  final double t;
  final double tc;
}

/// Online least-squares regression clock: fits `tcFrames(t) = a + b*t` over
/// a short sliding window of (capture-sample-time, timecode-frame-count)
/// observations. See this file's top-level doc comment for the design
/// rationale.
class LtcClockModel {
  LtcClockModel({
    this.maxAge = const Duration(seconds: 4),
    Duration? staleAfter,
    this.maxPoints = 120,
    this.minPointsForFit = 2,
    this.minPointsUsable = 8,
    this.minSpanUsableSeconds = 0.25,
    this.maxResidualUsableFrames = 0.75,
    this.jumpResetFrames = 5.0,
    Duration Function() elapsedSinceStart = monotonicMicrosNowAsDuration,
  })  : assert(maxPoints >= minPointsForFit),
        assert(minPointsUsable >= minPointsForFit),
        staleAfter = staleAfter ?? maxAge,
        _elapsedSinceStart = elapsedSinceStart;

  /// Points older than this (by sample-time distance from the newest point)
  /// are pruned from the sliding window. Deliberately a separate concern
  /// from [staleAfter] even though they default to the same value: this one
  /// governs how much regression history to keep (measured along the
  /// observations' own sample-time axis), not whether the source is still
  /// alive.
  final Duration maxAge;

  /// A source that has not produced any new observation for this long, by
  /// *monotonic* elapsed time (checked lazily on read via the injected
  /// `elapsedSinceStart` clock — see this file's "Monotonic timing" doc),
  /// is treated as stopped — see the class doc on staleness. Defaults to
  /// [maxAge] when not given explicitly, since "keep about 4s of history"
  /// and "a 4s silence means the source stopped" are both reasonable at the
  /// same default, but the two are independently configurable (a test, or a
  /// future caller, may want a short staleness timeout without also
  /// shrinking the regression window down to too few points).
  final Duration staleAfter;

  /// Hard cap on the sliding window's point count, independent of age.
  final int maxPoints;

  /// Minimum points before any line can be fit at all (2 — a line needs two
  /// distinct points). Below this, [measuredFrameRate], [residualRms] and
  /// [nominalFps] are `null`.
  final int minPointsForFit;

  /// Minimum points, combined with [minSpanUsableSeconds] and
  /// [maxResidualUsableFrames], before [usable] is `true`.
  final int minPointsUsable;

  /// Minimum window time span (newest point's `t` minus oldest's), in
  /// seconds, before [usable] is `true`. A two-point fit spanning 10 ms is
  /// technically "a line" but tells you nothing about the real frame rate.
  final double minSpanUsableSeconds;

  /// Maximum fit residual RMS, in frames, before [usable] is `true`.
  final double maxResidualUsableFrames;

  /// A new observation whose actual timecode differs from the current fit's
  /// prediction (after same-day-wrap correction, if `framesPerDay` was
  /// supplied) by more than this many frames is treated as a discontinuity:
  /// the window is reset and refitting starts fresh from that point, rather
  /// than blending old and new timelines.
  final double jumpResetFrames;

  final ListQueue<_Observation> _points = ListQueue<_Observation>();

  // Unwrap state for same-day TC rollover (see addObservation).
  double _dayOffsetFrames = 0;

  // Cached fit, recomputed whenever the window changes.
  double? _a;
  double? _b;
  double? _residualRms;

  // Monotonic elapsed time (see this file's "Monotonic timing" doc) at the
  // most recent observation, used only for staleness — never displayed.
  Duration? _lastObservationElapsed;
  final Duration Function() _elapsedSinceStart;

  // --- Host-time mapping -------------------------------------------------
  double? _hostOffsetMicros; // EWMA of hostMicros - sampleTime*1e6
  static const double _hostOffsetAlpha = 0.2;

  /// Number of observations currently retained in the sliding window.
  int get observationCount => _points.length;

  /// Newest point's `t` minus oldest's, in seconds. `0` with fewer than 2
  /// points.
  double get windowSpanSeconds =>
      _points.length < 2 ? 0 : _points.last.t - _points.first.t;

  bool get _stale {
    final last = _lastObservationElapsed;
    if (last == null) return true;
    return _elapsedSinceStart() - last > staleAfter;
  }

  bool get _hasFit =>
      !_stale && _points.length >= minPointsForFit && _b != null;

  /// The real, unrounded frame rate the source is currently running at (the
  /// fit's slope `b`), in frames per real second. This is deliberately
  /// never rounded — it is the only place varispeed or a non-integer
  /// nominal rate (23.976, 29.97) is visible. `null` until [minPointsForFit]
  /// fresh observations are available.
  double? get measuredFrameRate => _hasFit ? _b : null;

  /// Fit quality: root-mean-square residual of the window's points against
  /// the fitted line, in frames. `null` under the same conditions as
  /// [measuredFrameRate].
  double? get residualRms => _hasFit ? _residualRms : null;

  /// The nearest of 24/25/30 to [measuredFrameRate], but only when that
  /// candidate is within a sane tolerance (0.5 frames/sec) — otherwise
  /// `null` rather than guessing. 23.976 fps content correctly reports `24`
  /// here while [measuredFrameRate] keeps reporting the real ~23.976 value.
  int? get nominalFps {
    final rate = measuredFrameRate;
    if (rate == null) return null;
    const candidates = [24, 25, 30];
    var best = candidates.first;
    var bestDelta = (rate - best).abs();
    for (final candidate in candidates.skip(1)) {
      final delta = (rate - candidate).abs();
      if (delta < bestDelta) {
        best = candidate;
        bestDelta = delta;
      }
    }
    return bestDelta <= 0.5 ? best : null;
  }

  /// Whether the fit is trustworthy enough to drive predictions
  /// ([tcAtSampleTime], [tcAtHostMicros]): enough points, spanning enough
  /// real time, with a low enough residual, and not stale.
  bool get usable =>
      !_stale &&
      _points.length >= minPointsUsable &&
      windowSpanSeconds >= minSpanUsableSeconds &&
      _residualRms != null &&
      _residualRms! <= maxResidualUsableFrames;

  /// Predicted timecode frame count (fractional — sub-frame precision) at
  /// capture sample-clock time [t] (seconds). `null` unless [usable].
  double? tcAtSampleTime(double t) {
    if (!usable) return null;
    return _a! + _b! * t;
  }

  /// Adds one (sample-time, timecode-frame-count) observation.
  ///
  /// [sampleTime] is `sampleIndex / sampleRate` (seconds) — the capture
  /// sample clock at the moment the frame's sync word completed, per
  /// [LtcFrame.sampleIndex]'s doc.
  ///
  /// [tcFrames] is the same-day, zero-based frame index for the decoded
  /// frame (see [ltcFrameIndex]) — *not* unwrapped across midnight; this
  /// method does that itself.
  ///
  /// [framesPerDay], if supplied (see [ltcFramesPerDay]), lets this method
  /// tell a genuine midnight rollover (jump of very close to
  /// `framesPerDay`, in either direction) apart from a real discontinuity
  /// (operator relocated the transport, source changed) — anything else
  /// past [jumpResetFrames] resets the window. Without it, a midnight
  /// rollover cannot be distinguished from a discontinuity and will reset
  /// the window, which is the safe (if momentarily disruptive) fallback.
  void addObservation(double sampleTime, double tcFrames, {int? framesPerDay}) {
    // This call is itself fresh evidence the source is alive, regardless of
    // whether the model was stale a moment ago.
    _lastObservationElapsed = _elapsedSinceStart();

    final hasFitToCompareAgainst =
        _points.length >= minPointsForFit && _b != null;
    if (!hasFitToCompareAgainst) {
      // No fit to compare against yet: nothing to jam-detect. Seed (or
      // continue seeding) the window as-is.
      _pushPoint(sampleTime, tcFrames + _dayOffsetFrames);
      return;
    }

    final predicted = _a! + _b! * sampleTime;
    var candidate = tcFrames + _dayOffsetFrames;
    var diff = candidate - predicted;

    if (framesPerDay != null && framesPerDay > 0) {
      if ((diff + framesPerDay).abs() <= jumpResetFrames) {
        // Rolled forward past midnight: raw value dropped by ~framesPerDay
        // relative to what the fit expected.
        _dayOffsetFrames += framesPerDay;
        candidate += framesPerDay;
        diff = candidate - predicted;
      } else if ((diff - framesPerDay).abs() <= jumpResetFrames) {
        // Rolled backward past midnight (operator rewound across 00:00).
        _dayOffsetFrames -= framesPerDay;
        candidate -= framesPerDay;
        diff = candidate - predicted;
      }
    }

    if (diff.abs() > jumpResetFrames) {
      // A jam, not a blend: the operator relocated the transport or the
      // source changed. Start over from this point.
      _reset(keepHostMapping: true);
      _lastObservationElapsed = _elapsedSinceStart();
      _dayOffsetFrames = 0;
      _pushPoint(sampleTime, tcFrames);
      return;
    }

    _pushPoint(sampleTime, candidate);
  }

  void _pushPoint(double t, double tc) {
    _points.addLast(_Observation(t, tc));
    _prune();
    _refit();
  }

  void _prune() {
    while (_points.length > maxPoints) {
      _points.removeFirst();
    }
    if (_points.isEmpty) return;
    final newestT = _points.last.t;
    final maxAgeSeconds = maxAge.inMicroseconds / 1e6;
    while (_points.length > minPointsForFit &&
        (newestT - _points.first.t) > maxAgeSeconds) {
      _points.removeFirst();
    }
  }

  // Least squares, fit on *centered* coordinates (dt = t - meanT, dtc = tc -
  // meanTc) rather than the raw (t, tc) sums.
  //
  // The original implementation summed t, t*t and t*tc directly. Over a long
  // session `t` (capture sample-clock seconds since the decoder was
  // constructed) grows unbounded — a session started hours in has t ~ 1e4 to
  // 1e5. `sumTT` then holds t^2 ~ 1e8-1e10, and `n * sumTTc - sumT * sumTc`
  // subtracts two similarly huge quantities whose difference (the actual
  // signal — how far the points deviate from the mean) is many orders of
  // magnitude smaller. That is catastrophic cancellation: IEEE double only
  // carries ~15-17 significant decimal digits, and the operands here can
  // burn through most of them before the subtraction even happens, leaving
  // too few to represent the *difference* accurately. The measured frame
  // rate (`b`, expected to be stable to a small fraction of a percent) can
  // then drift or jitter as new points enter an already-long window, purely
  // from arithmetic noise, not real signal.
  //
  // Centering removes the problem: `dt` values are always small (bounded by
  // `maxAge`'s span, a few seconds), so `sumDtDt`/`sumDtDtc` never involve
  // the huge, nearly-cancelling terms above regardless of how long the
  // session has been running. `b` is fully recovered from that clean centered
  // fit; the intercept `a` (the fit's value at t=0) is then reconstructed
  // from `b` and the two means. See `test/ltc_clock_test.dart`'s "long
  // session" test for a synthetic session starting at t=100000s that would
  // fail under the uncentered formulation.
  void _refit() {
    final n = _points.length;
    if (n < minPointsForFit) {
      _a = null;
      _b = null;
      _residualRms = null;
      return;
    }
    var sumT = 0.0, sumTc = 0.0;
    for (final p in _points) {
      sumT += p.t;
      sumTc += p.tc;
    }
    final meanT = sumT / n;
    final meanTc = sumTc / n;

    var sumDtDt = 0.0, sumDtDtc = 0.0;
    for (final p in _points) {
      final dt = p.t - meanT;
      final dtc = p.tc - meanTc;
      sumDtDt += dt * dt;
      sumDtDtc += dt * dtc;
    }
    if (sumDtDt.abs() < 1e-9) {
      // All observations share (numerically) the same sample time — cannot
      // determine a slope. Keep any previous fit rather than dividing by
      // (near) zero.
      return;
    }
    final b = sumDtDtc / sumDtDt;
    final a = meanTc - b * meanT;
    var sumSq = 0.0;
    for (final p in _points) {
      final residual = p.tc - (a + b * p.t);
      sumSq += residual * residual;
    }
    _a = a;
    _b = b;
    _residualRms = math.sqrt(sumSq / n);
  }

  // --- Host-time mapping -------------------------------------------------

  /// Records that the host observed sample-clock time [sampleTime] (seconds
  /// — typically the currently-fed decoder's `sampleIndex / sampleRate` at
  /// the moment a PCM chunk was handed to it) arrive at host time
  /// [hostMicros]. **Not** `DateTime.now().microsecondsSinceEpoch` — see
  /// this file's top-level "Monotonic timing" doc: [hostMicros] must be
  /// [monotonicMicrosNow] (or another reading of the same process-lifetime
  /// [Stopwatch]), so it shares an epoch with [tcAtHostMicros]'s own
  /// argument and can't be skewed by a wall-clock step between calls.
  /// Maintains an exponentially-weighted estimate of the host-to-sample-clock
  /// offset so [tcAtHostMicros] can translate a host `Timer`-driven deadline
  /// into a sample-clock time / predicted timecode. See this file's
  /// top-level doc comment: this offset includes real capture latency and is
  /// not compensated here.
  void noteHostTime(double sampleTime, int hostMicros) {
    final offset = hostMicros - sampleTime * 1e6;
    final previous = _hostOffsetMicros;
    _hostOffsetMicros = previous == null
        ? offset
        : previous + _hostOffsetAlpha * (offset - previous);
  }

  /// Predicted timecode frame count at host time [hostMicros] — see
  /// [noteHostTime]'s doc: this must be [monotonicMicrosNow], not
  /// `DateTime.now().microsecondsSinceEpoch` — via the [noteHostTime] offset
  /// estimate and [tcAtSampleTime]. `null` until both a usable regression
  /// fit and at least one [noteHostTime] call are available.
  double? tcAtHostMicros(int hostMicros) {
    final offset = _hostOffsetMicros;
    if (offset == null) return null;
    final sampleTime = (hostMicros - offset) / 1e6;
    return tcAtSampleTime(sampleTime);
  }

  /// Explicit "the source is gone" signal — e.g. the caller's own
  /// heuristics (decoder unlocked for a sustained period, capture stopped)
  /// decided the signal is lost before this model's own [staleAfter]-based
  /// staleness would have caught it. Clears the window and the host-time
  /// mapping immediately so a stopped source never keeps reporting a
  /// running clock.
  void onSignalLost() => _reset(keepHostMapping: false);

  /// Full reset: clears the regression window, day-wrap offset and
  /// host-time mapping. Call this whenever the underlying sample-clock
  /// origin changes discontinuously in a way this model cannot itself infer
  /// from timecode content alone — e.g. capture restarted at a different
  /// sample rate (the decoder's `sampleIndex` restarts from 0 against a new
  /// clock), or the active channel changed.
  void reset() => _reset(keepHostMapping: false);

  void _reset({required bool keepHostMapping}) {
    _points.clear();
    _a = null;
    _b = null;
    _residualRms = null;
    _dayOffsetFrames = 0;
    _lastObservationElapsed = null;
    if (!keepHostMapping) {
      _hostOffsetMicros = null;
    }
  }
}
