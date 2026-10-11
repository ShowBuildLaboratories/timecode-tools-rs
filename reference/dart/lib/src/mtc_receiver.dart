part of 'receiver.dart';

/// Reads MTC (MIDI timecode) from a stream of raw MIDI bytes.
///
/// Understands quarter-frame messages (`F1 0nnn dddd`) and full-frame
/// SysEx messages (`F0 7F <device> 01 01 hh mm ss ff F7`). Everything else
/// in the stream (notes, clock, other SysEx, real-time bytes) is skipped.
///
/// ```dart
/// final rx = MtcReceiver();
/// rx.attach(midiStream); // Stream<MidiChunk>
/// rx.frames.listen((tc) => print(tc));
/// rx.events.listen((e) => print(e));
/// await rx.waitFor(Timecode.parse('01:00:10:00'));
/// await rx.detach();
/// ```
///
/// Eight quarter frames, sent over two frames, carry the frame that began at
/// the first of them. Readings come once per frame: at piece 7, the set's
/// second frame (began at piece 4); at piece 3 of the next set, the frame
/// after that (began at its piece 0), if the pieces so far agree with it.
/// The signal locks once two complete sets in a row were two frames apart.
///
/// A full-frame message (a locate) ends any chase ([StateEventType.lost] if
/// locked) and is reported as a [StateEventType.jump], with [current] set to
/// the new position.
///
/// The frame rate comes from the messages (24, 25, 29.97 drop-frame, 30)
/// unless [frameRate] fixes it.
class MtcReceiver extends _ReceiverBase<MidiChunk> {
  MtcReceiver({
    super.frameRate = FrameRate.auto,
    super.offsetFrames = 0,
    super.lostAfter = const Duration(milliseconds: 300),
    super.backpressure = Backpressure.latest,
    super.now,
  });

  /// Complete, in-order quarter-frame sets needed before [isLocked].
  static const int lockSets = 2;

  // --- byte parser ---
  int? _status; // status byte awaiting data
  int _dataNeeded = 0;
  int _dataSeen = 0;
  List<int>? _sysex; // bytes of a SysEx in progress (after F0)

  // --- quarter-frame assembly ---
  final List<int> _pieces = List<int>.filled(8, 0);
  int _expected = 0; // next piece number expected
  Duration _piece0At = Duration.zero;
  Duration _piece4At = Duration.zero;
  (int, FrameRate)? _lastSet; // frame index and rate of the last full set
  int _contiguousSets = 0;
  int? _lastReading;

  @override
  void _onChunk(MidiChunk chunk) {
    final at = chunk.timestamp ?? _now();
    for (final b in chunk.bytes) {
      _onByte(b, at);
    }
    _checkLoss(at.inMicroseconds / 1e6);
  }

  void _onByte(int b, Duration at) {
    if (b >= 0xF8) return; // real-time bytes may appear anywhere
    final sysex = _sysex;
    if (sysex != null) {
      if (b == 0xF7) {
        _sysex = null;
        _onSysex(sysex, at);
        return;
      }
      if (b < 0x80) {
        if (sysex.length < 32) sysex.add(b);
        return;
      }
      _sysex = null; // a status byte ends an unterminated SysEx
    }
    if (b >= 0x80) {
      _dataSeen = 0;
      if (b == 0xF0) {
        _sysex = <int>[];
        _status = null;
      } else if (b >= 0xF4) {
        _status = null; // no data bytes
      } else {
        _status = b;
        _dataNeeded = switch (b) {
          0xF1 || 0xF3 => 1,
          0xF2 => 2,
          >= 0xC0 && < 0xE0 => 1,
          _ => 2,
        };
      }
      return;
    }
    final status = _status;
    if (status == null) return; // stray data byte
    _dataSeen++;
    if (_dataSeen < _dataNeeded) return;
    _dataSeen = 0;
    if (status >= 0xF0) _status = null; // no running status for system
    if (status == 0xF1) _onQuarterFrame(b >> 4 & 0x07, b & 0x0F, at);
  }

  void _onQuarterFrame(int piece, int value, Duration at) {
    if (piece == 0) {
      _expected = 0;
      _piece0At = at;
    }
    if (piece != _expected) {
      // Out of order (dropped bytes or reverse playback): start over.
      _expected = 0;
      _contiguousSets = 0;
      _lastSet = null;
      _lastReading = null;
      return;
    }
    _pieces[piece] = value;
    _expected = (piece + 1) % 8;
    if (piece == 4) _piece4At = at;

    if (piece == 3) {
      // The frame after the last set's second frame began at this set's
      // piece 0. Report it if the pieces so far agree.
      final last = _lastSet;
      if (last != null) {
        final (index, rate) = last;
        final tc = Timecode.fromFrameIndex(index + 2, rate);
        if (_pieces[0] == tc.frames & 0x0F &&
            _pieces[1] & 0x01 == tc.frames >> 4 &&
            _pieces[2] == tc.seconds & 0x0F &&
            _pieces[3] & 0x03 == tc.seconds >> 4) {
          _reading(index + 2, rate, _piece0At);
        }
      }
    } else if (piece == 7) {
      final rate = _rateFromBits(_pieces[7] >> 1 & 0x03);
      final index = _indexOf(
        rate,
        _pieces[6] | (_pieces[7] & 0x01) << 4,
        _pieces[4] | (_pieces[5] & 0x03) << 4,
        _pieces[2] | (_pieces[3] & 0x03) << 4,
        _pieces[0] | (_pieces[1] & 0x01) << 4,
      );
      if (index == null) {
        _contiguousSets = 0;
        _lastSet = null;
        _lastReading = null;
        return;
      }
      final last = _lastSet;
      _contiguousSets = last != null &&
              last.$2 == rate &&
              (last.$1 + 2) % rate.framesPerDay == index
          ? _contiguousSets + 1
          : 1;
      _lastSet = (index, rate);
      _reading(index + 1, rate, _piece4At);
    }
  }

  void _reading(int sourceIndex, FrameRate sourceRate, Duration began) {
    final day = sourceRate.framesPerDay;
    final index = sourceIndex % day;
    final last = _lastReading;
    final follows = last != null && (last + 1) % day == index;
    _lastReading = index;
    final rate = frameRate != FrameRate.auto ? frameRate : sourceRate;
    final t = began.inMicroseconds / 1e6;
    _clock.noteHostTime(t, began.inMicroseconds);
    _clock.addObservation(t, index.toDouble(), framesPerDay: day);
    final label = Timecode.fromFrameIndex(index, sourceRate);
    final tc = Timecode.fromFrameIndex(
      Timecode(label.hours, label.minutes, label.seconds, label.frames)
              .frameIndex(rate) +
          offsetFrames,
      rate,
      timestamp: began,
    );
    _onReading(
      tc,
      t,
      sourceLocked: _contiguousSets >= lockSets,
      follows: follows,
    );
  }

  void _onSysex(List<int> s, Duration at) {
    // 7F <device> 01 01 hh mm ss ff
    if (s.length != 8 || s[0] != 0x7F || s[2] != 0x01 || s[3] != 0x01) return;
    final sourceRate = _rateFromBits(s[4] >> 5 & 0x03);
    final index = _indexOf(sourceRate, s[4] & 0x1F, s[5], s[6], s[7]);
    if (index == null) return;
    // A locate: quarter-frame counting starts over.
    _expected = 0;
    _contiguousSets = 0;
    _lastSet = null;
    _lastReading = null;
    _clock.reset();
    final rate = frameRate != FrameRate.auto ? frameRate : sourceRate;
    _locate(Timecode.fromFrameIndex(
      Timecode(s[4] & 0x1F, s[5], s[6], s[7]).frameIndex(rate) + offsetFrames,
      rate,
      timestamp: at,
    ));
  }

  static int? _indexOf(FrameRate rate, int h, int m, int s, int f) {
    final tc = Timecode.tryParse(
      '${_two(h)}:${_two(m)}:${_two(s)}:${_two(f)}',
      frameRate: rate,
    );
    return tc?.frameIndex(rate);
  }

  static String _two(int v) => v.toString().padLeft(2, '0');

  static FrameRate _rateFromBits(int bits) => switch (bits) {
        0 => FrameRate.fps24,
        1 => FrameRate.fps25,
        2 => FrameRate.fps2997Drop,
        _ => FrameRate.fps30,
      };
}
