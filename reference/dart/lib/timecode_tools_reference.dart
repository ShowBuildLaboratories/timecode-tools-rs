/// Pure-Dart SMPTE timecode reading: LTC from audio and MTC from MIDI.
///
/// - [LtcReceiver]: attach a `Stream<PcmChunk>` of audio.
/// - [MtcReceiver]: attach a `Stream<MidiChunk>` of raw MIDI bytes.
///
/// Both report [Timecode]s on `frames`, [StateEvent]s (lock / lost / jump /
/// end) on `events`, and offer `waitFor` and `positionAt`.
///
/// Also: [Timecode] and [FrameRate] (24, 25, 29.97 drop-frame, 30, or
/// auto), the low-level [LtcDecoder], and [encodeLtcSequence], a small
/// helper that synthesizes LTC audio for tests.
library;

export 'src/clock.dart';
export 'src/decoder.dart';
export 'src/encoder.dart';
export 'src/midi_chunk.dart';
export 'src/pcm_chunk.dart';
export 'src/receiver.dart';
export 'src/timecode.dart';
