/*
 * timecode_tools.h - C interface to the timecode-tools Rust core.
 *
 * SMPTE LTC and MIDI Time Code reading: receivers (lock / frame / jump /
 * lost), the low-level LTC decoder, the regression clock, timecode
 * arithmetic and a test-signal encoder.
 *
 * Conventions
 * - Every function is prefixed `tt_`. Handles are opaque and must be freed
 *   with the matching `*_free` function. A handle may be used from any
 *   thread, but not from two threads at once.
 * - Times are monotonic nanoseconds (int64_t). -1 means "none".
 * - Frame rates are TT_RATE_* values; TT_RATE_NONE (-1) means "detect from
 *   the source" in options and "unknown" in results.
 * - Functions returning int32_t return >= 0 on success (a count, or 0/1 for
 *   yes/no) and a negative TT_ERR_* code on failure.
 * - Arrays returned by `tt_*_events` / `tt_decoder_frames` belong to the
 *   handle and stay valid until the next call that changes that handle.
 *
 * MIT License, Copyright (c) 2026 Show Build Laboratories.
 */
#ifndef TIMECODE_TOOLS_H
#define TIMECODE_TOOLS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- Constants --------------------------------------------------------- */

/* Frame rates (the same numbering as the MTC rate bits). */
#define TT_RATE_NONE (-1)
#define TT_RATE_24 0
#define TT_RATE_25 1
#define TT_RATE_2997_DROP 2 /* 29.97 fps drop-frame */
#define TT_RATE_30 3

/* Receiver event kinds. */
#define TT_EVENT_LOCK 0  /* the source became stable; a FRAME follows */
#define TT_EVENT_FRAME 1 /* a frame on the followed timeline (while locked) */
#define TT_EVENT_JUMP 2  /* the timecode moved, or an MTC full-frame locate */
#define TT_EVENT_LOST 3  /* nothing followed for lost_after, or input stopped */

/* Error codes (negative return values). */
#define TT_ERR_NULL (-1)     /* a required pointer was NULL */
#define TT_ERR_ARGUMENT (-2) /* invalid sample rate, channel count or block length */
#define TT_ERR_CHANNEL (-3)  /* the selected channel is not in the input */
#define TT_ERR_PANIC (-4)    /* internal error (a bug); the handle is unusable */

/* ---- Version ----------------------------------------------------------- */

/* The core version, e.g. "0.1.0". Static; do not free. */
const char *tt_version(void);

/* ---- Timecode ---------------------------------------------------------- */

typedef struct TtTimecode {
    uint8_t hours;
    uint8_t minutes;
    uint8_t seconds;
    uint8_t frames;
    uint8_t drop_frame; /* 0 or 1 */
    int8_t frame_rate;  /* TT_RATE_*, or TT_RATE_NONE if unknown */
    uint8_t locked;     /* 1 if the source was locked when this was read */
    uint8_t reserved;
    int64_t timestamp_ns; /* when this timecode began (input clock), or -1 */
    int64_t sample_index; /* LTC: one-based sample of the frame's last edge, or -1 */
    int32_t channel;      /* LTC: one-based channel, or -1 */
    int32_t reserved2;
} TtTimecode;

/* Frames since 00:00:00:00 for a label at `fps` (24, 25 or 30). With
 * `drop_frame` the drop-frame numbering applies. No range checks. */
int64_t tt_frame_index(uint8_t hours, uint8_t minutes, uint8_t seconds, uint8_t frames,
                       int32_t drop_frame, uint32_t fps);

/* The label for a frame count, wrapping around one day. Fills hours,
 * minutes, seconds, frames and drop_frame; the other fields are set to
 * "none". Returns TT_ERR_ARGUMENT unless fps is 24, 25 or 30. */
int32_t tt_label_from_index(int64_t index, uint32_t fps, int32_t drop_frame, TtTimecode *out);

/* Frames in a 24-hour day, or TT_ERR_ARGUMENT for an fps other than 24/25/30. */
int64_t tt_frames_per_day(uint32_t fps, int32_t drop_frame);

/* ---- Receivers --------------------------------------------------------- */

typedef struct TtReceiver TtReceiver;

typedef struct TtReceiverOptions {
    int32_t frame_rate;     /* TT_RATE_*, or TT_RATE_NONE to take it from the source */
    int32_t channel;        /* LTC: one-based channel; 0 scans all channels */
    int64_t offset_frames;  /* added to every timecode (latency compensation) */
    int64_t lost_after_ns;  /* nothing followed this long -> LOST; <= 0 means 300 ms */
} TtReceiverOptions;

typedef struct TtEvent {
    int32_t kind;         /* TT_EVENT_* */
    int32_t has_previous; /* FRAME: 1 if it continues the previous reading */
    int64_t previous;     /* FRAME: frame index of that previous reading */
    TtTimecode timecode;  /* LOCK, FRAME, JUMP: the timecode; LOST: zeroed */
} TtEvent;

/* A receiver that follows LTC in PCM, or MTC in MIDI bytes. NULL options
 * means the defaults (detect the rate, offset 0, 300 ms, channel 1).
 * Returns NULL if the options are invalid. */
TtReceiver *tt_receiver_new_ltc(const TtReceiverOptions *options);
TtReceiver *tt_receiver_new_mtc(const TtReceiverOptions *options);
void tt_receiver_free(TtReceiver *receiver);

/* LTC: one block of interleaved PCM. `timestamp_ns` is the monotonic time
 * the first sample was captured (-1 if unknown). Returns the number of
 * events, read them with tt_receiver_events. Errors: TT_ERR_ARGUMENT,
 * TT_ERR_CHANNEL (the input has fewer channels than the one selected; the
 * block is ignored). Calling these on an MTC receiver returns TT_ERR_ARGUMENT. */
int32_t tt_receiver_push_pcm_i16(TtReceiver *receiver, const int16_t *samples, size_t len,
                                 uint32_t sample_rate, uint16_t channels, int64_t timestamp_ns);
int32_t tt_receiver_push_pcm_f32(TtReceiver *receiver, const float *samples, size_t len,
                                 uint32_t sample_rate, uint16_t channels, int64_t timestamp_ns);

/* MTC: raw MIDI bytes that arrived at `timestamp_ns` (required). Returns the
 * number of events. Calling this on an LTC receiver returns TT_ERR_ARGUMENT. */
int32_t tt_receiver_push_midi(TtReceiver *receiver, const uint8_t *bytes, size_t len,
                              int64_t timestamp_ns);

/* Ends the chase now (input stopped, detached or closed). Returns the number
 * of events (1 LOST if it was locked, otherwise 0). */
int32_t tt_receiver_signal_lost(TtReceiver *receiver);

/* The events of the last push / signal_lost call. */
const TtEvent *tt_receiver_events(const TtReceiver *receiver);

/* 1 if locked, 0 if not. */
int32_t tt_receiver_locked(const TtReceiver *receiver);

/* The rate in use (configured or detected), or TT_RATE_NONE. */
int32_t tt_receiver_detected_rate(const TtReceiver *receiver);

/* The most recent followed timecode (or MTC locate position): returns 1 and
 * fills `out`, or 0 if there is none. */
int32_t tt_receiver_current(const TtReceiver *receiver, TtTimecode *out);

/* LTC: recent peak input level 0..1 of the channel read. MTC: 0. */
double tt_receiver_level(const TtReceiver *receiver);

int64_t tt_receiver_offset_frames(const TtReceiver *receiver);
void tt_receiver_set_offset_frames(TtReceiver *receiver, int64_t offset_frames);

/* The position at `timestamp_ns` (the input clock) as a fractional frame
 * count since 00:00:00:00 (not wrapped at midnight), with the offset
 * applied, from a least-squares fit of the recent signal. Returns 1 and
 * fills `out`, or 0 until locked long enough to fit (or after loss). */
int32_t tt_receiver_frames_at(const TtReceiver *receiver, int64_t timestamp_ns, double *out);

/* ---- Low-level LTC decoder --------------------------------------------- */

typedef struct TtDecoder TtDecoder;

typedef struct TtLtcFrame {
    uint8_t hours;
    uint8_t minutes;
    uint8_t seconds;
    uint8_t frames;
    uint8_t drop_frame;
    uint8_t locked;        /* the decoder's lock state after this frame */
    int16_t estimated_fps; /* 24, 25 or 30 after this frame, or -1 */
    int64_t sample_index;  /* one-based sample of the frame's last edge */
} TtLtcFrame;

/* A mono PCM16 LTC decoder. Returns NULL if sample_rate is 0. */
TtDecoder *tt_decoder_new(uint32_t sample_rate);
void tt_decoder_free(TtDecoder *decoder);

/* Consecutive mono samples. Returns the number of frames completed. */
int32_t tt_decoder_push(TtDecoder *decoder, const int16_t *samples, size_t len);
/* Little-endian PCM16 bytes; an odd trailing byte is carried to the next
 * call. Do not mix with tt_decoder_push in one stream. */
int32_t tt_decoder_push_bytes(TtDecoder *decoder, const uint8_t *bytes, size_t len);
/* The frames of the last push call. */
const TtLtcFrame *tt_decoder_frames(const TtDecoder *decoder);

int32_t tt_decoder_locked(const TtDecoder *decoder);
int32_t tt_decoder_estimated_fps(const TtDecoder *decoder); /* 24/25/30, or -1 */
double tt_decoder_signal_level(const TtDecoder *decoder);   /* 0..1 */
int64_t tt_decoder_sample_index(const TtDecoder *decoder);  /* samples processed */

/* ---- Regression clock -------------------------------------------------- */

typedef struct TtClock TtClock;

typedef struct TtClockConfig {
    double max_age_seconds;      /* default 4 */
    int64_t stale_after_ns;      /* default 4 s */
    uint32_t max_points;         /* default 120 */
    uint32_t min_points_for_fit; /* default 2 */
    uint32_t min_points_usable;  /* default 8 */
    uint32_t reserved;
    double min_span_seconds;     /* default 0.25 */
    double max_residual_frames;  /* default 0.75 */
    double jump_reset_frames;    /* default 5 */
} TtClockConfig;

typedef struct TtClockState {
    int32_t has_fit;      /* a, b and residual are valid */
    int32_t usable;
    double a;             /* frames at sample time 0 */
    double b;             /* measured frames per second */
    double residual;      /* RMS residual in frames */
    int32_t nominal_fps;  /* 24, 25 or 30, or -1 */
    uint32_t observation_count;
    double window_span_seconds;
} TtClockState;

/* The default configuration. */
void tt_clock_default_config(TtClockConfig *out);
/* NULL config means the defaults. Returns NULL if the config is invalid. */
TtClock *tt_clock_new(const TtClockConfig *config);
void tt_clock_free(TtClock *clock);

/* One observation: timecode `tc` (frames) at `sample_time` seconds.
 * `frames_per_day` > 0 enables midnight unwrap. `now_ns` is the monotonic
 * time of the call, for staleness. */
void tt_clock_add_observation(TtClock *clock, double sample_time, double tc,
                              int64_t frames_per_day, int64_t now_ns);
/* Maps sample time to host time: the host clock read `host_micros` at
 * `sample_time` seconds (smoothed). */
void tt_clock_note_host_time(TtClock *clock, double sample_time, int64_t host_micros);
void tt_clock_state(const TtClock *clock, int64_t now_ns, TtClockState *out);
/* Returns 1 and fills `out` when usable at `now_ns`, else 0. */
int32_t tt_clock_tc_at_sample_time(const TtClock *clock, double sample_time, int64_t now_ns,
                                   double *out);
int32_t tt_clock_tc_at_host_micros(const TtClock *clock, int64_t host_micros, int64_t now_ns,
                                   double *out);
/* Both clear the observations and the host mapping. */
void tt_clock_reset(TtClock *clock);
void tt_clock_on_signal_lost(TtClock *clock);

/* ---- Test-signal encoder ----------------------------------------------- */

typedef struct TtSamples {
    int16_t *data;
    size_t len;
} TtSamples;

/* A square-wave LTC test signal (the reference encoder): `frame_count`
 * frames at `fps` (24/25/30, real rate equal to fps) from the given label.
 * Free with tt_samples_free. Returns {NULL, 0} on invalid arguments. */
TtSamples tt_encode_ltc(uint32_t sample_rate, uint32_t fps, int32_t drop_frame, uint8_t hours,
                        uint8_t minutes, uint8_t seconds, uint8_t frames, uint32_t frame_count,
                        int16_t amplitude, uint8_t preamble_bit);
void tt_samples_free(TtSamples samples);

#ifdef __cplusplus
}
#endif

#endif /* TIMECODE_TOOLS_H */
