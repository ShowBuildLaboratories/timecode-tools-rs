import 'package:test/test.dart';
import 'package:timecode_tools_reference/timecode_tools_reference.dart';

void main() {
  group('LtcClockModel', () {
    test(
        'exact 30fps observations: sub-frame interpolation, measured rate '
        'and nominal fps resolve', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps; // 1600

      // 40 consecutive, perfectly regular observations — a real locked
      // 30fps source with no jitter.
      for (var i = 0; i < 40; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }

      expect(model.usable, isTrue);
      expect(model.measuredFrameRate, closeTo(30.0, 0.01));
      expect(model.nominalFps, 30);
      expect(model.residualRms, lessThan(0.01));

      // Sub-frame interpolation: halfway between frame 20 and frame 21's
      // sample time should predict tc very close to 20.5.
      const tAtFrame20 = (20 * framePeriodSamples) / sampleRate;
      const tHalfway = tAtFrame20 + (framePeriodSamples / 2) / sampleRate;
      expect(model.tcAtSampleTime(tHalfway), closeTo(20.5, 0.05));
    });

    test(
        '23.976 fps (24000/1001) distinguishes itself from true 24.0 while '
        'nominalFps still resolves to 24', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const realFps = 24000 / 1001; // 23.976023...
      const framePeriodSamples = sampleRate / realFps;

      for (var i = 0; i < 40; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }

      expect(model.usable, isTrue);
      expect(model.measuredFrameRate, isNotNull);
      // The raw measured rate is exposed unrounded and is clearly not 24.0.
      expect(model.measuredFrameRate!, closeTo(realFps, 0.01));
      expect((model.measuredFrameRate! - 24.0).abs(), greaterThan(0.005));
      // But the nearest nominal bucket is still 24.
      expect(model.nominalFps, 24);
    });

    test(
        'jittered observation sample times (+/-20ms) still produce a '
        'small, stable-residual fit', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;
      // Fixed pseudo-random jitter pattern (deterministic, not flaky):
      // alternate +/-20ms with a couple of intermediate values.
      const jitterMs = [-20, 12, -8, 20, -15, 5, -20, 18, -3, 20];

      for (var i = 0; i < 40; i++) {
        final nominalT = (i * framePeriodSamples) / sampleRate;
        final jitterS = jitterMs[i % jitterMs.length] / 1000.0;
        model.addObservation(nominalT + jitterS, i.toDouble());
      }

      expect(model.usable, isTrue);
      // Regression should still recover close to the true 30fps rate...
      expect(model.measuredFrameRate, closeTo(30.0, 0.5));
      // ...with a residual that stays sub-frame despite the jitter.
      expect(model.residualRms, lessThan(0.5));

      // Predictions must be stable: two calls at the same time agree
      // exactly (deterministic fit, not recomputed per call in a way that
      // could drift).
      final p1 = model.tcAtSampleTime(1.0);
      final p2 = model.tcAtSampleTime(1.0);
      expect(p1, equals(p2));
    });

    test('0.5% varispeed is tracked by measuredFrameRate', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const realFps = 30.0 * 1.005; // 30.15 — 0.5% fast
      const framePeriodSamples = sampleRate / realFps;

      for (var i = 0; i < 40; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }

      expect(model.usable, isTrue);
      expect(model.measuredFrameRate, closeTo(realFps, 0.02));
      // Still nominally a 30fps source (within tolerance), just running
      // hot — nominalFps must not be confused into reporting null or 25.
      expect(model.nominalFps, 30);
    });

    test(
        'a TC jump of more than 5 frames resets the window instead of '
        'blending old and new timelines', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;

      for (var i = 0; i < 20; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }
      expect(model.usable, isTrue);
      expect(model.tcAtSampleTime(19 * framePeriodSamples / sampleRate),
          closeTo(19.0, 0.05));

      // Operator relocates the transport far away (100 frames forward) —
      // sample-clock time keeps advancing continuously (still the same
      // capture device), but the decoded tc jumps discontinuously.
      const jamStartT = 20 * framePeriodSamples / sampleRate;
      const jamStartTc = 500.0;
      model.addObservation(jamStartT, jamStartTc);

      // Immediately after a single jam point there are too few points to
      // be usable yet — this proves the window was actually cleared, not
      // blended (a blend would still have >=8 points from before and stay
      // "usable" with a wild slope).
      expect(model.usable, isFalse);

      // Feed the new timeline; the model should fit *only* the new
      // timeline, not a blend of the old (~30fps starting near tc=0) and
      // new (starting near tc=500) data.
      for (var i = 1; i < 20; i++) {
        final t = jamStartT + (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, jamStartTc + i);
      }

      expect(model.usable, isTrue);
      expect(model.measuredFrameRate, closeTo(30.0, 0.05));
      final predicted = model.tcAtSampleTime(
        jamStartT + (19 * framePeriodSamples) / sampleRate,
      );
      expect(predicted, closeTo(jamStartTc + 19, 0.5));
    });

    test(
        'staleness: no new observations for longer than maxAge makes the '
        'clock unusable', () async {
      var elapsed = Duration.zero;
      final model = LtcClockModel(
        staleAfter: const Duration(milliseconds: 50),
        elapsedSinceStart: () => elapsed,
      );
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;

      for (var i = 0; i < 20; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }
      expect(model.usable, isTrue);

      // Advance the injected clock past maxAge without any further
      // observations arriving — e.g. the source stopped.
      elapsed = elapsed + const Duration(milliseconds: 200);

      expect(model.usable, isFalse);
      expect(model.tcAtSampleTime(1.0), isNull);
      expect(model.measuredFrameRate, isNull);
    });

    test(
        'staleness is immune to a backward wall-clock (NTP) jump: the '
        'injected monotonic clock, not DateTime.now(), governs it', () async {
      // Regression test for the wall-clock staleness bug: previously this
      // model's staleness check compared DateTime.now() to the wall-clock
      // time of the last observation. A backward NTP step between two reads
      // would make elapsed wall time appear negative (or just wrong),
      // either hiding a genuinely stale source or falsely flagging a live
      // one. Using a monotonic elapsed-time source instead means staleness
      // only ever depends on real elapsed time, never on what DateTime.now()
      // happens to report.
      var elapsed = const Duration(seconds: 100000); // arbitrary large base
      final model = LtcClockModel(
        staleAfter: const Duration(milliseconds: 50),
        elapsedSinceStart: () => elapsed,
      );
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;

      for (var i = 0; i < 20; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }
      expect(model.usable, isTrue);

      // Real time only advances by 10ms (well under staleAfter) between
      // reads — a wall-clock DateTime.now() implementation could report
      // something wildly different here after an NTP step, but the
      // monotonic clock cannot.
      elapsed = elapsed + const Duration(milliseconds: 10);
      expect(model.usable, isTrue);
    });

    test('onSignalLost() immediately clears usability', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;
      for (var i = 0; i < 20; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }
      expect(model.usable, isTrue);

      model.onSignalLost();

      expect(model.usable, isFalse);
      expect(model.observationCount, 0);
    });

    test(
        'host-time mapping: tcAtHostMicros translates a host wall-clock '
        'timestamp through the sample-time fit', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;

      const captureLatencyMicros = 40000; // ~1 chunk, as documented
      const hostEpochStart = 1000000000; // arbitrary base

      for (var i = 0; i < 20; i++) {
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
        final hostMicros =
            hostEpochStart + (t * 1e6).round() + captureLatencyMicros;
        model.noteHostTime(t, hostMicros);
      }

      expect(model.usable, isTrue);
      const tAtFrame10 = (10 * framePeriodSamples) / sampleRate;
      final hostAtFrame10 =
          hostEpochStart + (tAtFrame10 * 1e6).round() + captureLatencyMicros;
      final predicted = model.tcAtHostMicros(hostAtFrame10);
      expect(predicted, closeTo(10.0, 0.05));
    });

    group('ltcFrameIndex (drop-frame conversion)', () {
      test('non-drop-frame: naive fps*seconds+frames math', () {
        expect(
          ltcFrameIndex(
            hours: 1,
            minutes: 0,
            seconds: 0,
            frames: 0,
            dropFrame: false,
            fps: 30,
          ),
          30 * 3600,
        );
      });

      test(
          'drop-frame: exactly one hour of drop-frame TC covers exactly '
          'one real hour at 29.97 fps (the textbook identity)', () {
        final oneHour = ltcFrameIndex(
          hours: 1,
          minutes: 0,
          seconds: 0,
          frames: 0,
          dropFrame: true,
          fps: 30,
        );
        // 29.97 * 3600 == 107892 exactly (2997/100 * 3600).
        expect(oneHour, 107892);
      });

      test('drop-frame: 00:00:00;00 is frame index 0', () {
        expect(
          ltcFrameIndex(
            hours: 0,
            minutes: 0,
            seconds: 0,
            frames: 0,
            dropFrame: true,
            fps: 30,
          ),
          0,
        );
      });

      test(
          'ltcFramesPerDay round-trips against ltcFrameIndex at the last '
          'valid frame of the day', () {
        const fps = 30;
        for (final dropFrame in [false, true]) {
          final lastValidFrame = ltcFrameIndex(
            hours: 23,
            minutes: 59,
            seconds: 59,
            frames: fps - 1,
            dropFrame: dropFrame,
            fps: fps,
          );
          expect(
            ltcFramesPerDay(fps: fps, dropFrame: dropFrame),
            lastValidFrame + 1,
          );
        }
        // Non-drop-frame: exactly 24h worth of frames at nominal fps.
        expect(ltcFramesPerDay(fps: 30, dropFrame: false), 24 * 3600 * 30);
        // Drop-frame: fewer, because frame numbers were skipped.
        expect(
          ltcFramesPerDay(fps: 30, dropFrame: true),
          lessThan(24 * 3600 * 30),
        );
      });
    });

    test(
        'addObservation unwraps a real midnight rollover instead of '
        'treating it as a discontinuity when framesPerDay is supplied', () {
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30;
      const framesPerDay = 24 * 3600 * 30; // non-drop-frame
      const framePeriodSamples = sampleRate / fps;

      // Run right up to the end of the day.
      const lastOfDay = framesPerDay - 1;
      for (var i = 0; i < 20; i++) {
        final tc = (lastOfDay - 19 + i).toDouble();
        final t = (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, tc, framesPerDay: framesPerDay);
      }
      expect(model.usable, isTrue);

      // The next real frame wraps to 00:00:00:00 (same-day frame index 0).
      const wrapT = 20 * framePeriodSamples / sampleRate;
      model.addObservation(wrapT, 0.0, framesPerDay: framesPerDay);

      // A real wrap must NOT be treated as a jam: the window keeps its
      // prior points, so it is still (or immediately becomes again) usable
      // once enough points span it, and the predicted (unwrapped) tc keeps
      // climbing continuously across the boundary rather than resetting.
      final predictedAtWrap = model.tcAtSampleTime(wrapT);
      expect(predictedAtWrap, isNotNull);
      expect(predictedAtWrap!, closeTo(framesPerDay.toDouble(), 1.0));
    });

    test(
        'long session: centered least squares keeps slope precision when '
        'observations start far from t=0', () {
      // Regression test for catastrophic cancellation in the old uncentered
      // least-squares sums (sum(t), sum(t*t), sum(t*tc)): those grow with
      // t^2, so once a session has been running for a while (t ~ 1e5
      // seconds — a few hours in), the *signal* (how far points deviate
      // from the mean, which is what the slope actually depends on) gets
      // buried under IEEE double rounding error in numbers that are many
      // orders of magnitude larger. Fitting on centered coordinates
      // (dt = t - meanT) keeps every sum bounded by the window's own span
      // regardless of how large t itself has grown, so precision should not
      // degrade at all here.
      final model = LtcClockModel();
      const sampleRate = 48000;
      const fps = 30.0;
      const framePeriodSamples = sampleRate / fps;
      const startT = 100000.0; // seconds — hours into a real session

      for (var i = 0; i < 40; i++) {
        final t = startT + (i * framePeriodSamples) / sampleRate;
        model.addObservation(t, i.toDouble());
      }

      expect(model.usable, isTrue);
      // Exact, noise-free samples: the true rate should be recovered to
      // essentially full double precision, not just "close enough for
      // display."
      expect(model.measuredFrameRate, closeTo(30.0, 1e-6));
      expect(model.residualRms, lessThan(1e-6));

      const tAtFrame20 = startT + (20 * framePeriodSamples) / sampleRate;
      expect(model.tcAtSampleTime(tAtFrame20), closeTo(20.0, 1e-6));
    });
  });
}
