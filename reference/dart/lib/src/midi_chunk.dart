import 'dart:typed_data';

/// Raw MIDI bytes for `MtcReceiver.attach`, as delivered by a MIDI input.
class MidiChunk {
  /// [bytes] may hold any MIDI traffic (several messages, or part of one —
  /// a SysEx message may be split across chunks); only MTC is read.
  ///
  /// [timestamp] is when the bytes arrived, on a monotonic clock (one that
  /// never jumps). Leave it `null` if the MIDI API does not provide one:
  /// the receiver then uses the time it received the chunk. For accurate
  /// timing, deliver each quarter-frame message in its own chunk with its
  /// own timestamp, as most MIDI APIs do.
  const MidiChunk(this.bytes, {this.timestamp});

  final Uint8List bytes;
  final Duration? timestamp;
}
