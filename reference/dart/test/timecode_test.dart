import 'package:test/test.dart';
import 'package:timecode_tools_reference/timecode_tools_reference.dart';

void main() {
  group('ltcTimecodeStringFromFrameIndex (inverse of ltcFrameIndex)', () {
    test('round-trips every frame of a 24h day at 30 fps drop-frame', () {
      const fps = 30;
      final perDay = ltcFramesPerDay(fps: fps, dropFrame: true);
      for (var index = 0; index < perDay; index++) {
        final text = ltcTimecodeStringFromFrameIndex(
            index: index, fps: fps, dropFrame: true);
        final hh = int.parse(text.substring(0, 2));
        final mm = int.parse(text.substring(3, 5));
        final ss = int.parse(text.substring(6, 8));
        final ff = int.parse(text.substring(9, 11));
        expect(text[8], ';');
        expect(
          ltcFrameIndex(
              hours: hh,
              minutes: mm,
              seconds: ss,
              frames: ff,
              dropFrame: true,
              fps: fps),
          index,
          reason: 'index $index rendered as $text',
        );
      }
    });

    test('round-trips sampled frames at 24/25/30 non-drop', () {
      for (final fps in [24, 25, 30]) {
        final perDay = ltcFramesPerDay(fps: fps, dropFrame: false);
        for (var index = 0; index < perDay; index += 7919) {
          final text = ltcTimecodeStringFromFrameIndex(
              index: index, fps: fps, dropFrame: false);
          final hh = int.parse(text.substring(0, 2));
          final mm = int.parse(text.substring(3, 5));
          final ss = int.parse(text.substring(6, 8));
          final ff = int.parse(text.substring(9, 11));
          expect(text[8], ':');
          expect(
            ltcFrameIndex(
                hours: hh,
                minutes: mm,
                seconds: ss,
                frames: ff,
                dropFrame: false,
                fps: fps),
            index,
            reason: 'fps $fps index $index rendered as $text',
          );
        }
      }
    });

    test('textbook drop-frame identities', () {
      // 29.97 * 3600 == 107892 frames == exactly 01:00:00;00 DF.
      expect(
          ltcTimecodeStringFromFrameIndex(
              index: 107892, fps: 30, dropFrame: true),
          '01:00:00;02'.replaceRange(9, 11, '00'));
      // Minute boundary skips frame numbers 0 and 1 (non-tenth minute).
      expect(
          ltcTimecodeStringFromFrameIndex(
              index: ltcFrameIndex(
                      hours: 0,
                      minutes: 0,
                      seconds: 59,
                      frames: 29,
                      dropFrame: true,
                      fps: 30) +
                  1,
              fps: 30,
              dropFrame: true),
          '00:01:00;02');
      // Tenth minutes do not drop.
      expect(
          ltcTimecodeStringFromFrameIndex(
              index: ltcFrameIndex(
                      hours: 0,
                      minutes: 9,
                      seconds: 59,
                      frames: 29,
                      dropFrame: true,
                      fps: 30) +
                  1,
              fps: 30,
              dropFrame: true),
          '00:10:00;00');
      // Wraps at the 24h day, negatives wrap backward.
      expect(
          ltcTimecodeStringFromFrameIndex(
              index: ltcFramesPerDay(fps: 25, dropFrame: false),
              fps: 25,
              dropFrame: false),
          '00:00:00:00');
      expect(
          ltcTimecodeStringFromFrameIndex(index: -1, fps: 25, dropFrame: false),
          '23:59:59:24');
    });
  });
}
