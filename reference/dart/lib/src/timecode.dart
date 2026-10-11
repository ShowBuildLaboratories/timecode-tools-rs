/// Pure SMPTE timecode arithmetic shared by the decoder (bit stream
/// decoding, frame-to-frame lock/contiguity checks) and the sample-clock
/// regression model.
library;

/// Converts one SMPTE hh:mm:ss(:|;)ff timecode field set into a same-day,
/// zero-based frame index, correctly accounting for drop-frame's skipped
/// frame numbers.
///
/// [fps] is the *nominal* encoding rate (24, 25 or 30) as carried in the
/// LTC bitstream's frame-count field width — for drop-frame timecode this
/// is always 30 (drop-frame represents a real rate of 29.97 fps; the
/// bitstream's frame field still counts 0..29, it just skips frame numbers
/// 0 and 1 at the start of most minutes to keep the displayed clock
/// tracking real time). This is the standard NTSC drop-frame algorithm
/// (verified against the textbook identity that exactly one hour of
/// drop-frame timecode covers exactly one real hour at 29.97 fps: this
/// function returns exactly `107892` for `01:00:00;00`, and `29.97 * 3600
/// == 107892` exactly).
int ltcFrameIndex({
  required int hours,
  required int minutes,
  required int seconds,
  required int frames,
  required bool dropFrame,
  required int fps,
}) {
  final totalMinutes = hours * 60 + minutes;
  final framesNoDrop =
      ((hours * 3600) + (minutes * 60) + seconds) * fps + frames;
  if (!dropFrame) return framesNoDrop;
  // 2 frame numbers dropped per minute at 30 fps; scales for other nominal
  // rates in case a future source ever pairs drop-frame with a rate other
  // than 30 (not currently emitted by any real device this app targets).
  final dropFramesPerMinute = (fps * 2) ~/ 30;
  final dropped = dropFramesPerMinute * (totalMinutes - (totalMinutes ~/ 10));
  return framesNoDrop - dropped;
}

/// Inverse of [ltcFrameIndex]: converts a same-day zero-based frame index
/// back into SMPTE timecode digits, re-inserting drop-frame's skipped frame
/// numbers. Formats as `HH:MM:SS:FF` (`;FF` separator when [dropFrame]).
///
/// This is what makes an extrapolated LTC clock displayable in the SOURCE's
/// own timebase: round-trips exactly with [ltcFrameIndex] for every frame
/// of a 24-hour day (unit-tested). [index] is wrapped into the day first.
String ltcTimecodeStringFromFrameIndex({
  required int index,
  required int fps,
  required bool dropFrame,
}) {
  final perDay = ltcFramesPerDay(fps: fps, dropFrame: dropFrame);
  var frameIndex = index % perDay;
  if (frameIndex < 0) frameIndex += perDay;
  int totalMinutes;
  int numberInMinute; // 0..(60*fps - 1), in TC digit numbering
  if (!dropFrame) {
    final framesPerMinute = 60 * fps;
    totalMinutes = frameIndex ~/ framesPerMinute;
    numberInMinute = frameIndex % framesPerMinute;
  } else {
    // Standard NTSC drop-frame inverse: minute 0 of each 10-minute block
    // keeps all 60*fps numbers; minutes 1..9 skip the first `drop` numbers.
    final drop = (fps * 2) ~/ 30;
    final framesPerFullMinute = 60 * fps;
    final framesPerDroppedMinute = framesPerFullMinute - drop;
    final framesPerTenMinutes =
        framesPerFullMinute + 9 * framesPerDroppedMinute;
    final tenMinuteBlocks = frameIndex ~/ framesPerTenMinutes;
    final withinBlock = frameIndex % framesPerTenMinutes;
    if (withinBlock < framesPerFullMinute) {
      totalMinutes = tenMinuteBlocks * 10;
      numberInMinute = withinBlock;
    } else {
      final afterFirst = withinBlock - framesPerFullMinute;
      totalMinutes =
          tenMinuteBlocks * 10 + 1 + afterFirst ~/ framesPerDroppedMinute;
      numberInMinute = afterFirst % framesPerDroppedMinute + drop;
    }
  }
  final ff = numberInMinute % fps;
  final ss = numberInMinute ~/ fps;
  final hh = totalMinutes ~/ 60;
  final mm = totalMinutes % 60;
  final separator = dropFrame ? ';' : ':';
  String two(int value) => value.toString().padLeft(2, '0');
  return '${two(hh)}:${two(mm)}:${two(ss)}$separator${two(ff)}';
}

/// The frame index one past the last valid frame of a 24-hour day for
/// [fps]/[dropFrame] — i.e. the value at which [ltcFrameIndex] wraps back
/// to 0. Callers that need to unwrap a same-day rollover (a real midnight
/// crossing, as opposed to a discontinuity) expect this value.
int ltcFramesPerDay({required int fps, required bool dropFrame}) {
  return ltcFrameIndex(
        hours: 23,
        minutes: 59,
        seconds: 59,
        frames: fps - 1,
        dropFrame: dropFrame,
        fps: fps,
      ) +
      1;
}

/// The frame rate to decode at. [auto] detects it from the signal.
enum FrameRate {
  /// Detect from the signal (bit rate, frame numbers and drop-frame flag).
  auto(0, false),
  fps24(24, false),
  fps25(25, false),

  /// 29.97 fps drop-frame (frame numbers count 0..29; `;` separator).
  fps2997Drop(30, true),
  fps30(30, false);

  const FrameRate(this.nominal, this.dropFrame);

  /// Frame numbers per second (30 for 29.97 drop-frame; 0 for [auto]).
  final int nominal;

  /// Whether this is drop-frame timecode.
  final bool dropFrame;

  /// Real frames per second (29.97… for drop-frame; 0 for [auto]).
  double get fps => dropFrame ? nominal * 1000 / 1001 : nominal.toDouble();

  /// Display label: `24`, `25`, `29.97DF`, `30` (or `auto`).
  String get label => switch (this) {
        FrameRate.auto => 'auto',
        FrameRate.fps2997Drop => '29.97DF',
        _ => '$nominal',
      };

  /// Frames in a 24-hour day at this rate.
  int get framesPerDay {
    _checkConcrete();
    return ltcFramesPerDay(fps: nominal, dropFrame: dropFrame);
  }

  /// The concrete rate for a decoded nominal rate (24/25/30) and drop-frame
  /// flag, or `null` if [fps] is not one of 24/25/30.
  static FrameRate? fromDecoded({required int? fps, required bool dropFrame}) {
    if (dropFrame) {
      return fps == 30 || fps == null ? FrameRate.fps2997Drop : null;
    }
    return switch (fps) {
      24 => FrameRate.fps24,
      25 => FrameRate.fps25,
      30 => FrameRate.fps30,
      _ => null,
    };
  }

  void _checkConcrete() {
    if (this == FrameRate.auto) {
      throw StateError('FrameRate.auto has no concrete frame count');
    }
  }
}

/// A SMPTE timecode `HH:MM:SS:FF` (`HH:MM:SS;FF` for drop-frame).
///
/// [frameRate] is the rate the label is read at (`null` if unknown).
/// [timestamp], when set by a receiver, is the moment (on the chunk
/// timestamp clock) at which this frame began. Neither is part of equality.
class Timecode implements Comparable<Timecode> {
  const Timecode(
    this.hours,
    this.minutes,
    this.seconds,
    this.frames, {
    this.dropFrame = false,
    this.frameRate,
    this.timestamp,
  });

  final int hours;
  final int minutes;
  final int seconds;
  final int frames;
  final bool dropFrame;

  /// The rate this timecode is read at; `null` if unknown.
  final FrameRate? frameRate;

  final Duration? timestamp;

  static const Timecode zero = Timecode(0, 0, 0, 0);

  static final RegExp _pattern =
      RegExp(r'^(\d{1,2}):(\d{1,2}):(\d{1,2})([:;.,])(\d{1,2})$');

  /// Parses `HH:MM:SS:FF`. A `;`, `.` or `,` before the frames marks
  /// drop-frame. With a concrete [frameRate], the drop-frame flag follows
  /// the rate and the frame number must exist at that rate. Throws
  /// [FormatException] when malformed or out of range.
  factory Timecode.parse(String text, {FrameRate? frameRate}) {
    final tc = tryParse(text, frameRate: frameRate);
    if (tc == null) throw FormatException('Invalid timecode', text);
    return tc;
  }

  /// Like [Timecode.parse], but returns `null` instead of throwing.
  static Timecode? tryParse(String? text, {FrameRate? frameRate}) {
    final tc = _parse(text);
    if (tc == null || frameRate == null || frameRate == FrameRate.auto) {
      return tc;
    }
    if (tc.dropFrame && !frameRate.dropFrame) return null;
    if (tc.frames >= frameRate.nominal) return null;
    if (frameRate.dropFrame &&
        tc.seconds == 0 &&
        tc.minutes % 10 != 0 &&
        tc.frames < 2) {
      return null; // skipped in drop-frame timecode
    }
    return Timecode(tc.hours, tc.minutes, tc.seconds, tc.frames,
        dropFrame: frameRate.dropFrame, frameRate: frameRate);
  }

  static Timecode? _parse(String? text) {
    if (text == null) return null;
    final m = _pattern.firstMatch(text.trim());
    if (m == null) return null;
    final h = int.parse(m.group(1)!);
    final mi = int.parse(m.group(2)!);
    final s = int.parse(m.group(3)!);
    final f = int.parse(m.group(5)!);
    if (h > 23 || mi > 59 || s > 59 || f > 29) return null;
    return Timecode(h, mi, s, f, dropFrame: m.group(4) != ':');
  }

  /// The timecode [index] frames after 00:00:00:00 at [rate] (wrapped into
  /// the day; negative values wrap backward).
  factory Timecode.fromFrameIndex(
    int index,
    FrameRate rate, {
    Duration? timestamp,
  }) {
    final perDay = rate.framesPerDay;
    var i = index % perDay;
    if (i < 0) i += perDay;
    final text = ltcTimecodeStringFromFrameIndex(
      index: i,
      fps: rate.nominal,
      dropFrame: rate.dropFrame,
    );
    final tc = tryParse(text)!;
    return Timecode(
      tc.hours,
      tc.minutes,
      tc.seconds,
      tc.frames,
      dropFrame: rate.dropFrame,
      frameRate: rate,
      timestamp: timestamp,
    );
  }

  /// Frames since 00:00:00:00 at [rate] (default: [frameRate]). A frame
  /// number beyond the rate (for example `:29` at 25 fps) counts as the
  /// last frame of that second.
  int frameIndex([FrameRate? rate]) => _frameIndex(
      rate ?? frameRate ?? (throw StateError('frameIndex needs a frame rate')));

  int _frameIndex(FrameRate rate) => ltcFrameIndex(
        hours: hours,
        minutes: minutes,
        seconds: seconds,
        frames: frames.clamp(0, rate.nominal - 1),
        dropFrame: rate.dropFrame,
        fps: rate.nominal,
      );

  /// This timecode moved by [count] frames at [rate] (wraps at midnight).
  Timecode addFrames(int count, FrameRate rate) =>
      Timecode.fromFrameIndex(frameIndex(rate) + count, rate,
          timestamp: timestamp);

  int get _key => ((hours * 60 + minutes) * 60 + seconds) * 100 + frames;

  /// Orders by the digits (ignores the drop-frame flag and [timestamp]).
  @override
  int compareTo(Timecode other) => _key.compareTo(other._key);

  @override
  bool operator ==(Object other) =>
      other is Timecode && other._key == _key && other.dropFrame == dropFrame;

  @override
  int get hashCode => Object.hash(_key, dropFrame);

  @override
  String toString() {
    String two(int v) => v.toString().padLeft(2, '0');
    return '${two(hours)}:${two(minutes)}:${two(seconds)}'
        '${dropFrame ? ';' : ':'}${two(frames)}';
  }
}
