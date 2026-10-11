import 'dart:async';
import 'dart:math' as math;
import 'dart:typed_data';

import 'clock.dart';
import 'decoder.dart';
import 'midi_chunk.dart';
import 'pcm_chunk.dart';
import 'timecode.dart';

part 'ltc_receiver.dart';
part 'mtc_receiver.dart';

/// The kinds of [StateEvent] (the same names as the Node.js events).
enum StateEventType {
  /// Several consecutive frames arrived in order: the timecode is trusted.
  /// The first frame on [TimecodeReceiver.frames] follows.
  lock,

  /// Nothing followed for [TimecodeReceiver.lostAfter], the stream was
  /// detached or ended, the input format changed, or (MTC) a full-frame
  /// locate stopped the chase.
  lost,

  /// While locked, the timecode moved discontinuously (confirmed by two
  /// contiguous readings; a single stray frame is ignored), or (MTC) a
  /// full-frame message set a new position.
  jump,

  /// The attached input stream finished (after a [lost], if it was locked).
  end,
}

/// A state change on [TimecodeReceiver.events].
class StateEvent {
  const StateEvent(this.type, {this.timecode, this.frameRate, this.channel});

  final StateEventType type;

  /// [StateEventType.lock]: the first followed frame. [StateEventType.jump]:
  /// the new position. Otherwise `null`.
  final Timecode? timecode;

  /// [StateEventType.lock]: the rate in use.
  final FrameRate? frameRate;

  /// [StateEventType.lock] from [LtcReceiver]: the channel read (1-based).
  final int? channel;

  @override
  String toString() => switch (type) {
        StateEventType.lock => 'lock $timecode ${frameRate?.label}',
        StateEventType.jump => 'jump $timecode',
        _ => type.name,
      };
}

/// What a paused listener of [TimecodeReceiver.frames] receives.
enum Backpressure {
  /// Keep only the newest frame while paused.
  latest,

  /// Queue every frame while paused.
  all,
}

/// The common interface of [LtcReceiver] (audio, [PcmChunk]) and
/// [MtcReceiver] (MIDI, [MidiChunk]): attach a stream of input chunks [T],
/// get [frames], [events], [waitFor] and [positionAt].
///
/// ```dart
/// Future<void> follow(TimecodeReceiver<Object?> rx) async {
///   rx.events.listen(print);
///   await for (final tc in rx.frames) {
///     print(tc);
///   }
/// }
/// ```
abstract interface class TimecodeReceiver<T> {
  /// The frame rate to use; [FrameRate.auto] takes it from the signal.
  FrameRate get frameRate;

  /// Latency compensation in frames, added to every reported timecode.
  /// Positive when the signal reaches you late (the usual case): the
  /// receiver then reports a later timecode than the one it just read.
  int get offsetFrames;
  set offsetFrames(int value);

  /// What [frames] keeps for a listener that is paused (for example an
  /// `await for` loop whose body is still running). [Backpressure.latest]
  /// (the default) keeps only the newest frame; [Backpressure.all] queues
  /// every frame. [events] always queues every event.
  Backpressure get backpressure;

  /// How long without a frame before a locked signal is
  /// [StateEventType.lost].
  Duration get lostAfter;

  /// Every frame on the followed timeline while locked (with [offsetFrames]
  /// applied and [Timecode.timestamp] set to when the frame began). Each
  /// listener gets its own subscription.
  Stream<Timecode> get frames;

  /// Lock, loss and jump events, and [StateEventType.end] when the attached
  /// input finishes. An error from the input stream is delivered as an
  /// error on this stream.
  Stream<StateEvent> get events;

  /// Whether the signal is currently locked.
  bool get isLocked;

  /// The frame rate in use ([frameRate], or the detected one); `null` until
  /// known.
  FrameRate? get detectedRate;

  /// The most recent frame on [frames], or the position of an MTC locate.
  Timecode? get current;

  /// Whether a stream is attached.
  bool get isAttached;

  /// Whether [close] has been called.
  bool get isClosed;

  /// Starts reading [input]. Throws [StateError] if a stream is already
  /// attached ([detach] it first).
  void attach(Stream<T> input);

  /// Stops reading (the input stream is cancelled). Reports
  /// [StateEventType.lost] if the signal was locked. Pending [waitFor] calls
  /// stay pending; attach another stream to continue.
  Future<void> detach();

  /// Completes when the received timecode (with [offsetFrames]) passes
  /// [target] during normal playback: the first frame at or after [target]
  /// that directly continues a frame before it. A jump over (or onto)
  /// [target] does not count. Fails with a [StateError] if the receiver is
  /// closed first.
  Future<Timecode> waitFor(Timecode target);

  /// The timecode at [timestamp] (same clock as the chunk timestamps),
  /// interpolated or extrapolated from the recent signal, with
  /// [offsetFrames] applied. `null` until the signal is locked long enough
  /// to fit a clock, or after it is lost.
  Timecode? positionAt(Duration timestamp);

  /// [positionAt] as a fractional frame count since 00:00:00:00 (not
  /// wrapped at midnight).
  double? framesAt(Duration timestamp);

  /// Detaches, closes [frames] and [events], and fails pending [waitFor]
  /// calls with a [StateError]. The receiver cannot be used again.
  Future<void> close();
}

/// Shared implementation of [TimecodeReceiver]. The lock, jump and loss
/// rules are the same as in the Rust/Node.js `timecode-tools`.
abstract class _ReceiverBase<T> implements TimecodeReceiver<T> {
  _ReceiverBase({
    required this.frameRate,
    required this.offsetFrames,
    required this.lostAfter,
    required this.backpressure,
    Duration Function()? now,
  }) : _now = now ?? monotonicMicrosNowAsDuration;

  @override
  final FrameRate frameRate;

  @override
  int offsetFrames;

  @override
  final Duration lostAfter;

  @override
  final Backpressure backpressure;

  final Duration Function() _now;

  final List<_FrameSink> _frameSinks = [];
  bool _closed = false;
  final StreamController<StateEvent> _events =
      StreamController<StateEvent>.broadcast();
  StreamSubscription<T>? _sub;
  final LtcClockModel _clock = LtcClockModel();
  final List<_Waiter> _waiters = [];

  // Lock / jump / loss state. Clocks are in seconds on the receiver's own
  // time base (sample time for LTC, chunk timestamps for MTC).
  bool _locked = false;
  FrameRate? _rate;
  Timecode? _current;
  (int, double)? _read; // the last reading of any kind
  (int, double) _followed = (0, 0); // the last reading followed while locked
  (int, double)? _pending; // a reading off the followed timeline
  Timer? _watchdog;
  final Stopwatch _sinceChunk = Stopwatch();
  Duration _chunkGap = Duration.zero;

  @override
  Stream<Timecode> get frames {
    late final _FrameSink sink;
    final c = StreamController<Timecode>(
      onCancel: () => _frameSinks.remove(sink),
      onResume: () => sink.flush(),
    );
    sink = _FrameSink(c, backpressure);
    if (_closed) {
      c.close();
    } else {
      _frameSinks.add(sink);
    }
    return c.stream;
  }

  @override
  Stream<StateEvent> get events => _events.stream;

  @override
  bool get isLocked => _locked;

  @override
  FrameRate? get detectedRate =>
      frameRate != FrameRate.auto ? frameRate : _rate;

  @override
  Timecode? get current => _current;

  @override
  bool get isAttached => _sub != null;

  @override
  bool get isClosed => _closed;

  @override
  void attach(Stream<T> input) {
    if (_closed) throw StateError('Receiver closed');
    if (_sub != null) {
      throw StateError('A stream is already attached; detach() it first');
    }
    late final StreamSubscription<T> sub;
    sub = input.listen(
      (chunk) {
        _onChunk(chunk);
        _armWatchdog();
      },
      onError: (Object error, StackTrace stack) {
        if (identical(_sub, sub) && !_events.isClosed) {
          _events.addError(error, stack);
        }
      },
      onDone: () {
        if (identical(_sub, sub)) {
          _sub = null;
          _lost();
          _emitEvent(const StateEvent(StateEventType.end));
        }
      },
    );
    _sub = sub;
  }

  @override
  Future<void> detach() async {
    final sub = _sub;
    _sub = null;
    await sub?.cancel();
    _lost();
  }

  @override
  Future<Timecode> waitFor(Timecode target) {
    if (_closed) return Future.error(StateError('Receiver closed'));
    final w = _Waiter(target);
    _waiters.add(w);
    return w.completer.future;
  }

  @override
  Timecode? positionAt(Duration timestamp) {
    final rate = _rate;
    final f = framesAt(timestamp);
    if (rate == null || f == null) return null;
    return Timecode.fromFrameIndex(f.floor(), rate, timestamp: timestamp);
  }

  @override
  double? framesAt(Duration timestamp) {
    if (_rate == null || !_locked) return null;
    final f = _clock.tcAtHostMicros(timestamp.inMicroseconds);
    return f == null ? null : f + offsetFrames;
  }

  @override
  Future<void> close() async {
    if (_closed) return;
    await detach();
    _closed = true;
    _watchdog?.cancel();
    for (final sink in List.of(_frameSinks)) {
      sink.close();
    }
    _frameSinks.clear();
    for (final w in _waiters) {
      if (!w.completer.isCompleted) {
        w.completer.completeError(StateError('Receiver closed'));
      }
    }
    _waiters.clear();
    await _events.close();
  }

  // --- for subclasses ----------------------------------------------------

  void _onChunk(T chunk);

  /// The LTC channel, for [StateEvent.channel].
  int? get _channel => null;

  void _emitEvent(StateEvent e) {
    if (!_events.isClosed) _events.add(e);
  }

  /// One reading [tc] (offset applied, [Timecode.frameRate] set) at [clock]
  /// seconds. [sourceLocked]: the source's own lock state. [follows]: the
  /// source says this is the frame right after its previous reading (MTC);
  /// otherwise the expected step comes from elapsed time, so a frame lost
  /// to a dropout or replaced by a corrupt one does not break continuity.
  void _onReading(
    Timecode tc,
    double clock, {
    required bool sourceLocked,
    bool follows = false,
  }) {
    final rate = tc.frameRate!;
    _rate = rate;
    final index = tc.frameIndex(rate);
    final day = rate.framesPerDay;
    int next((int, double) from) {
      final step =
          follows ? 1 : math.max(1, ((clock - from.$2) * rate.fps).round());
      return (from.$1 + step) % day;
    }

    final read = _read;
    final continuesRead = read != null && next(read) == index ? read.$1 : null;
    _read = (index, clock);
    if (!_locked) {
      if (!sourceLocked) return;
      _locked = true;
      _pending = null;
      _follow(tc, index, clock);
      _emitEvent(StateEvent(StateEventType.lock,
          timecode: tc, frameRate: rate, channel: _channel));
      _emitFrame(tc, continuesRead);
      return;
    }
    final pending = _pending;
    if (index == next(_followed)) {
      final previous = _followed.$1;
      _pending = null;
      _follow(tc, index, clock);
      _emitFrame(tc, previous);
    } else if (pending != null && index == next(pending)) {
      // Two contiguous readings on a new timeline confirm a jump; a single
      // stray one is ignored.
      _pending = null;
      _follow(tc, index, clock);
      _emitEvent(StateEvent(StateEventType.jump, timecode: tc));
      _emitFrame(tc, null);
    } else {
      _pending = (index, clock);
    }
  }

  void _follow(Timecode tc, int index, double clock) {
    _current = tc;
    _followed = (index, clock);
  }

  /// A position given outright (MTC full frame): ends any chase and reports
  /// the new position as a jump.
  void _locate(Timecode tc) {
    _lost();
    _current = tc;
    _rate = tc.frameRate ?? _rate;
    _read = null;
    _emitEvent(StateEvent(StateEventType.jump, timecode: tc));
  }

  /// Reports loss if nothing was followed for [lostAfter] before [now]
  /// (seconds, same time base as [_onReading]).
  void _checkLoss(double now) {
    if (_locked && now - _followed.$2 > lostAfter.inMicroseconds / 1e6) {
      _lost();
    }
  }

  void _lost() {
    _watchdog?.cancel();
    _watchdog = null;
    _pending = null;
    _read = null;
    if (!_locked) return;
    _locked = false;
    _clock.onSignalLost();
    _emitEvent(const StateEvent(StateEventType.lost));
  }

  // Also detects loss when chunks stop arriving altogether: no chunk for
  // [lostAfter] plus twice the usual gap between chunks (so input delivered
  // in large blocks is not mistaken for a stop).
  void _armWatchdog() {
    if (_sinceChunk.isRunning) {
      final gap = _sinceChunk.elapsed;
      _chunkGap =
          gap > const Duration(seconds: 5) ? const Duration(seconds: 5) : gap;
    }
    _sinceChunk
      ..reset()
      ..start();
    _watchdog?.cancel();
    _watchdog = null;
    if (!_locked || _closed) return;
    _watchdog = Timer(lostAfter + _chunkGap * 2, _lost);
  }

  void _emitFrame(Timecode tc, int? previous) {
    for (final sink in List.of(_frameSinks)) {
      sink.add(tc);
    }
    if (previous == null || _waiters.isEmpty) return;
    final rate = tc.frameRate!;
    final day = rate.framesPerDay;
    final index = tc.frameIndex(rate);
    final step = (index - previous) % day;
    if (step <= 0) return;
    _waiters.removeWhere((w) {
      final target = Timecode.tryParse(w.target.toString(), frameRate: rate);
      if (target == null) return false;
      if ((index - target.frameIndex(rate)) % day < step) {
        w.completer.complete(tc);
        return true;
      }
      return false;
    });
  }
}

class _Waiter {
  _Waiter(this.target);
  final Timecode target;
  final Completer<Timecode> completer = Completer<Timecode>();
}

class _FrameSink {
  _FrameSink(this.controller, this.mode);
  final StreamController<Timecode> controller;
  final Backpressure mode;
  Timecode? _pending;

  void add(Timecode tc) {
    if (controller.isClosed) return;
    if (mode == Backpressure.latest && controller.isPaused) {
      _pending = tc;
      return;
    }
    controller.add(tc);
  }

  void flush() {
    final p = _pending;
    _pending = null;
    if (p != null && !controller.isClosed) controller.add(p);
  }

  void close() {
    flush();
    controller.close();
  }
}
