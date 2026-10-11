/* Builds against include/timecode_tools.h and the library, and checks the
 * layout and a full decode. Run by scripts/check-c-abi.sh. */
#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include "timecode_tools.h"

int main(void) {
    printf("layout TtTimecode %zu %zu %zu %zu\n", sizeof(TtTimecode),
           offsetof(TtTimecode, timestamp_ns), offsetof(TtTimecode, sample_index),
           offsetof(TtTimecode, channel));
    printf("layout TtEvent %zu %zu\n", sizeof(TtEvent), offsetof(TtEvent, timecode));
    printf("layout TtReceiverOptions %zu %zu\n", sizeof(TtReceiverOptions),
           offsetof(TtReceiverOptions, offset_frames));
    printf("layout TtLtcFrame %zu %zu\n", sizeof(TtLtcFrame), offsetof(TtLtcFrame, sample_index));
    printf("layout TtClockConfig %zu %zu\n", sizeof(TtClockConfig),
           offsetof(TtClockConfig, min_span_seconds));
    printf("layout TtClockState %zu %zu\n", sizeof(TtClockState),
           offsetof(TtClockState, window_span_seconds));

    printf("version %s\n", tt_version());

    /* 50 frames of 25 fps from 01:00:00:00, decoded in 10 ms blocks. */
    TtSamples s = tt_encode_ltc(48000, 25, 0, 1, 0, 0, 0, 50, 20000, 1);
    assert(s.data != NULL && s.len > 0);
    TtReceiverOptions opt = {TT_RATE_NONE, 1, 2, 0};
    TtReceiver *rx = tt_receiver_new_ltc(&opt);
    assert(rx != NULL);
    int frames = 0, locks = 0;
    char last[16] = "";
    for (size_t i = 0; i < s.len; i += 480) {
        size_t n = s.len - i < 480 ? s.len - i : 480;
        int32_t count = tt_receiver_push_pcm_i16(rx, s.data + i, n, 48000, 1,
                                                 (int64_t)i * 1000000000 / 48000);
        assert(count >= 0);
        const TtEvent *ev = tt_receiver_events(rx);
        for (int32_t k = 0; k < count; k++) {
            if (ev[k].kind == TT_EVENT_LOCK) locks++;
            if (ev[k].kind == TT_EVENT_FRAME) {
                frames++;
                snprintf(last, sizeof last, "%02u:%02u:%02u:%02u", ev[k].timecode.hours,
                         ev[k].timecode.minutes, ev[k].timecode.seconds, ev[k].timecode.frames);
            }
        }
    }
    double pos = 0;
    int has = tt_receiver_frames_at(rx, (int64_t)s.len * 1000000000 / 48000, &pos);
    printf("receiver locks=%d frames=%d last=%s rate=%d frames_at=%d %.3f\n", locks, frames,
           last, tt_receiver_detected_rate(rx), has, pos);
    assert(locks == 1 && frames == 47 && strcmp(last, "01:00:02:01") == 0);
    assert(tt_receiver_detected_rate(rx) == TT_RATE_25 && has == 1);
    assert(tt_receiver_push_midi(rx, NULL, 0, 0) == TT_ERR_ARGUMENT);
    assert(tt_receiver_signal_lost(rx) == 1 && tt_receiver_events(rx)[0].kind == TT_EVENT_LOST);
    tt_receiver_free(rx);

    TtDecoder *d = tt_decoder_new(48000);
    int32_t n = tt_decoder_push(d, s.data, s.len);
    const TtLtcFrame *f = tt_decoder_frames(d);
    printf("decoder frames=%d first=%02u:%02u:%02u:%02u fps=%d locked=%d\n", n, f[0].hours,
           f[0].minutes, f[0].seconds, f[0].frames, tt_decoder_estimated_fps(d),
           tt_decoder_locked(d));
    assert(n == 50 && tt_decoder_estimated_fps(d) == 25);
    tt_decoder_free(d);
    tt_samples_free(s);

    TtClockConfig cfg;
    tt_clock_default_config(&cfg);
    TtClock *c = tt_clock_new(&cfg);
    for (int i = 0; i < 40; i++) tt_clock_add_observation(c, i / 30.0, i, 0, 0);
    TtClockState st;
    tt_clock_state(c, 0, &st);
    printf("clock usable=%d b=%.6f nominal=%d count=%u\n", st.usable, st.b, st.nominal_fps,
           st.observation_count);
    assert(st.usable == 1 && st.nominal_fps == 30 && st.has_fit == 1);
    /* 5 s later with no new observation: stale, so no fit (as the reference). */
    tt_clock_state(c, 5000000000LL, &st);
    assert(st.has_fit == 0 && st.usable == 0 && st.nominal_fps == -1);
    tt_clock_free(c);

    TtTimecode tc;
    assert(tt_label_from_index(17982, 30, 1, &tc) == 0 && tc.minutes == 10 && tc.drop_frame == 1);
    assert(tt_frame_index(1, 0, 0, 0, 1, 30) == 107892);
    assert(tt_frames_per_day(25, 0) == 2160000);
    puts("ok");
    return 0;
}
