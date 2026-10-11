//! C interface to `timecode-tools`. See `include/timecode_tools.h`.
//!
//! Every exported function catches panics (returning `TT_ERR_PANIC` or a
//! neutral value) so that no unwinding crosses the C boundary.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;
use timecode_tools::{
    clock::{Clock, ClockConfig},
    decoder::{DecodedFrame, Decoder},
    label::{frames_per_day, Label},
    ltc::{LtcError, PcmChunk, Samples},
    receiver::{
        LtcReceiver, MidiChunk, MtcReceiver, ReceiverEvent, ReceiverOptions, TimecodeReceiver,
    },
    synth::{generate, SynthConfig},
    timecode::{FrameRate, Timecode},
};

pub const TT_RATE_NONE: i32 = -1;
pub const TT_EVENT_LOCK: i32 = 0;
pub const TT_EVENT_FRAME: i32 = 1;
pub const TT_EVENT_JUMP: i32 = 2;
pub const TT_EVENT_LOST: i32 = 3;
pub const TT_ERR_NULL: i32 = -1;
pub const TT_ERR_ARGUMENT: i32 = -2;
pub const TT_ERR_CHANNEL: i32 = -3;
pub const TT_ERR_PANIC: i32 = -4;

fn rate_code(rate: Option<FrameRate>) -> i32 {
    match rate {
        None => TT_RATE_NONE,
        Some(FrameRate::Fps24) => 0,
        Some(FrameRate::Fps25) => 1,
        Some(FrameRate::Fps2997Drop) => 2,
        Some(FrameRate::Fps30) => 3,
    }
}

fn rate_from_code(code: i32) -> Result<Option<FrameRate>, ()> {
    match code {
        TT_RATE_NONE => Ok(None),
        0 => Ok(Some(FrameRate::Fps24)),
        1 => Ok(Some(FrameRate::Fps25)),
        2 => Ok(Some(FrameRate::Fps2997Drop)),
        3 => Ok(Some(FrameRate::Fps30)),
        _ => Err(()),
    }
}

fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

fn opt_ns(ns: i64) -> Option<u64> {
    (ns >= 0).then_some(ns as u64)
}

// ---- Timecode --------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TtTimecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: u8,
    pub frame_rate: i8,
    pub locked: u8,
    pub reserved: u8,
    pub timestamp_ns: i64,
    pub sample_index: i64,
    pub channel: i32,
    pub reserved2: i32,
}

impl TtTimecode {
    fn label(label: Label) -> Self {
        Self {
            hours: label.hours,
            minutes: label.minutes,
            seconds: label.seconds,
            frames: label.frames,
            drop_frame: u8::from(label.drop_frame),
            frame_rate: TT_RATE_NONE as i8,
            locked: 0,
            reserved: 0,
            timestamp_ns: -1,
            sample_index: -1,
            channel: -1,
            reserved2: 0,
        }
    }

    fn from(tc: &Timecode) -> Self {
        Self {
            frame_rate: rate_code(tc.frame_rate) as i8,
            locked: u8::from(tc.locked),
            timestamp_ns: tc.timestamp_ns.map_or(-1, |t| t as i64),
            sample_index: tc.sample_index.map_or(-1, |s| s as i64),
            channel: tc.channel.map_or(-1, |c| c as i32),
            ..Self::label(tc.label)
        }
    }
}

#[no_mangle]
pub extern "C" fn tt_version() -> *const c_char {
    static VERSION: &CStr = c"0.1.0";
    debug_assert_eq!(VERSION.to_str().unwrap(), timecode_tools::VERSION);
    VERSION.as_ptr()
}

fn valid_fps(fps: u32) -> bool {
    matches!(fps, 24 | 25 | 30)
}

#[no_mangle]
pub extern "C" fn tt_frame_index(
    hours: u8,
    minutes: u8,
    seconds: u8,
    frames: u8,
    drop_frame: i32,
    fps: u32,
) -> i64 {
    guard(0, || {
        if fps == 0 {
            return 0;
        }
        Label {
            hours,
            minutes,
            seconds,
            frames,
            drop_frame: drop_frame != 0,
        }
        .frame_index(fps)
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_label_from_index(
    index: i64,
    fps: u32,
    drop_frame: i32,
    out: *mut TtTimecode,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        if out.is_null() {
            return TT_ERR_NULL;
        }
        if !valid_fps(fps) {
            return TT_ERR_ARGUMENT;
        }
        *out = TtTimecode::label(Label::from_index(index, fps, drop_frame != 0));
        0
    })
}

#[no_mangle]
pub extern "C" fn tt_frames_per_day(fps: u32, drop_frame: i32) -> i64 {
    guard(i64::from(TT_ERR_PANIC), || {
        if !valid_fps(fps) {
            return i64::from(TT_ERR_ARGUMENT);
        }
        frames_per_day(fps, drop_frame != 0)
    })
}

// ---- Receivers -------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TtReceiverOptions {
    pub frame_rate: i32,
    pub channel: i32,
    pub offset_frames: i64,
    pub lost_after_ns: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TtEvent {
    pub kind: i32,
    pub has_previous: i32,
    pub previous: i64,
    pub timecode: TtTimecode,
}

enum Inner {
    Ltc(LtcReceiver),
    Mtc(MtcReceiver),
}

pub struct TtReceiver {
    inner: Inner,
    events: Vec<TtEvent>,
}

impl TtReceiver {
    fn receiver(&self) -> &dyn ReceiverState {
        match &self.inner {
            Inner::Ltc(r) => r,
            Inner::Mtc(r) => r,
        }
    }

    fn store(&mut self, events: Vec<ReceiverEvent>) -> i32 {
        self.events = events
            .into_iter()
            .map(|e| match e {
                ReceiverEvent::Lock(tc) => TtEvent {
                    kind: TT_EVENT_LOCK,
                    timecode: TtTimecode::from(&tc),
                    ..Default::default()
                },
                ReceiverEvent::Frame { timecode, previous } => TtEvent {
                    kind: TT_EVENT_FRAME,
                    has_previous: i32::from(previous.is_some()),
                    previous: previous.unwrap_or(0),
                    timecode: TtTimecode::from(&timecode),
                },
                ReceiverEvent::Jump(tc) => TtEvent {
                    kind: TT_EVENT_JUMP,
                    timecode: TtTimecode::from(&tc),
                    ..Default::default()
                },
                ReceiverEvent::Lost => TtEvent {
                    kind: TT_EVENT_LOST,
                    ..Default::default()
                },
            })
            .collect();
        self.events.len() as i32
    }
}

/// Object-safe view of the parts of `TimecodeReceiver` that do not depend
/// on the chunk type.
trait ReceiverState {
    fn locked(&self) -> bool;
    fn detected_rate(&self) -> Option<FrameRate>;
    fn current(&self) -> Option<Timecode>;
    fn offset_frames(&self) -> i64;
    fn frames_at(&self, timestamp_ns: u64) -> Option<f64>;
}

impl<R: TimecodeReceiver> ReceiverState for R {
    fn locked(&self) -> bool {
        TimecodeReceiver::locked(self)
    }
    fn detected_rate(&self) -> Option<FrameRate> {
        TimecodeReceiver::detected_rate(self)
    }
    fn current(&self) -> Option<Timecode> {
        TimecodeReceiver::current(self)
    }
    fn offset_frames(&self) -> i64 {
        TimecodeReceiver::offset_frames(self)
    }
    fn frames_at(&self, timestamp_ns: u64) -> Option<f64> {
        TimecodeReceiver::frames_at(self, timestamp_ns)
    }
}

unsafe fn options(ptr: *const TtReceiverOptions) -> Option<ReceiverOptions> {
    let mut o = ReceiverOptions::default();
    if let Some(opts) = ptr.as_ref() {
        o.frame_rate = rate_from_code(opts.frame_rate).ok()?;
        o.offset_frames = opts.offset_frames;
        if opts.lost_after_ns > 0 {
            o.lost_after = Duration::from_nanos(opts.lost_after_ns as u64);
        }
        o.channel = match opts.channel {
            0 => None,
            c if (1..=64).contains(&c) => Some(c as u8),
            _ => return None,
        };
    }
    Some(o)
}

fn boxed(inner: Inner) -> *mut TtReceiver {
    Box::into_raw(Box::new(TtReceiver {
        inner,
        events: Vec::new(),
    }))
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_new_ltc(
    options_ptr: *const TtReceiverOptions,
) -> *mut TtReceiver {
    guard(std::ptr::null_mut(), || match options(options_ptr) {
        Some(o) => boxed(Inner::Ltc(LtcReceiver::new(o))),
        None => std::ptr::null_mut(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_new_mtc(
    options_ptr: *const TtReceiverOptions,
) -> *mut TtReceiver {
    guard(std::ptr::null_mut(), || match options(options_ptr) {
        Some(o) => boxed(Inner::Mtc(MtcReceiver::new(o))),
        None => std::ptr::null_mut(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_free(receiver: *mut TtReceiver) {
    if !receiver.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(receiver))));
    }
}

unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> Option<&'a [T]> {
    if len == 0 {
        Some(&[])
    } else if ptr.is_null() {
        None
    } else {
        Some(std::slice::from_raw_parts(ptr, len))
    }
}

fn ltc_error(e: LtcError) -> i32 {
    match e {
        LtcError::Channel { .. } => TT_ERR_CHANNEL,
        _ => TT_ERR_ARGUMENT,
    }
}

unsafe fn push_pcm(
    receiver: *mut TtReceiver,
    samples: Option<Samples<'_>>,
    sample_rate: u32,
    channels: u16,
    timestamp_ns: i64,
) -> i32 {
    let Some(rx) = receiver.as_mut() else {
        return TT_ERR_NULL;
    };
    let Some(samples) = samples else {
        return TT_ERR_NULL;
    };
    let Inner::Ltc(ltc) = &mut rx.inner else {
        return TT_ERR_ARGUMENT;
    };
    let chunk = PcmChunk {
        samples,
        sample_rate,
        channels,
        timestamp_ns: opt_ns(timestamp_ns),
    };
    match ltc.push(chunk) {
        Ok(events) => rx.store(events),
        Err(e) => {
            rx.events.clear();
            ltc_error(e)
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_push_pcm_i16(
    receiver: *mut TtReceiver,
    samples: *const i16,
    len: usize,
    sample_rate: u32,
    channels: u16,
    timestamp_ns: i64,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let s = slice(samples, len).map(Samples::I16);
        push_pcm(receiver, s, sample_rate, channels, timestamp_ns)
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_push_pcm_f32(
    receiver: *mut TtReceiver,
    samples: *const f32,
    len: usize,
    sample_rate: u32,
    channels: u16,
    timestamp_ns: i64,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let s = slice(samples, len).map(Samples::F32);
        push_pcm(receiver, s, sample_rate, channels, timestamp_ns)
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_push_midi(
    receiver: *mut TtReceiver,
    bytes: *const u8,
    len: usize,
    timestamp_ns: i64,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let Some(rx) = receiver.as_mut() else {
            return TT_ERR_NULL;
        };
        let Some(data) = slice(bytes, len) else {
            return TT_ERR_NULL;
        };
        let Inner::Mtc(mtc) = &mut rx.inner else {
            return TT_ERR_ARGUMENT;
        };
        if timestamp_ns < 0 {
            return TT_ERR_ARGUMENT;
        }
        let chunk = MidiChunk {
            data,
            timestamp_ns: timestamp_ns as u64,
        };
        match mtc.push(chunk) {
            Ok(events) => rx.store(events),
            Err(never) => match never {},
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_signal_lost(receiver: *mut TtReceiver) -> i32 {
    guard(TT_ERR_PANIC, || {
        let Some(rx) = receiver.as_mut() else {
            return TT_ERR_NULL;
        };
        let events = match &mut rx.inner {
            Inner::Ltc(r) => r.signal_lost(),
            Inner::Mtc(r) => r.signal_lost(),
        };
        rx.store(events)
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_events(receiver: *const TtReceiver) -> *const TtEvent {
    match receiver.as_ref() {
        Some(rx) if !rx.events.is_empty() => rx.events.as_ptr(),
        _ => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_locked(receiver: *const TtReceiver) -> i32 {
    guard(TT_ERR_PANIC, || match receiver.as_ref() {
        Some(rx) => i32::from(rx.receiver().locked()),
        None => TT_ERR_NULL,
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_detected_rate(receiver: *const TtReceiver) -> i32 {
    guard(TT_RATE_NONE, || match receiver.as_ref() {
        Some(rx) => rate_code(rx.receiver().detected_rate()),
        None => TT_RATE_NONE,
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_current(
    receiver: *const TtReceiver,
    out: *mut TtTimecode,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let (Some(rx), false) = (receiver.as_ref(), out.is_null()) else {
            return TT_ERR_NULL;
        };
        match rx.receiver().current() {
            Some(tc) => {
                *out = TtTimecode::from(&tc);
                1
            }
            None => 0,
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_level(receiver: *const TtReceiver) -> f64 {
    guard(0., || match receiver.as_ref().map(|r| &r.inner) {
        Some(Inner::Ltc(r)) => r.level(),
        _ => 0.,
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_offset_frames(receiver: *const TtReceiver) -> i64 {
    guard(0, || {
        receiver
            .as_ref()
            .map_or(0, |r| r.receiver().offset_frames())
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_set_offset_frames(
    receiver: *mut TtReceiver,
    offset_frames: i64,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        match receiver.as_mut().map(|r| &mut r.inner) {
            Some(Inner::Ltc(r)) => r.set_offset_frames(offset_frames),
            Some(Inner::Mtc(r)) => r.set_offset_frames(offset_frames),
            None => {}
        }
    }));
}

#[no_mangle]
pub unsafe extern "C" fn tt_receiver_frames_at(
    receiver: *const TtReceiver,
    timestamp_ns: i64,
    out: *mut f64,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let (Some(rx), false) = (receiver.as_ref(), out.is_null()) else {
            return TT_ERR_NULL;
        };
        if timestamp_ns < 0 {
            return 0;
        }
        match rx.receiver().frames_at(timestamp_ns as u64) {
            Some(f) => {
                *out = f;
                1
            }
            None => 0,
        }
    })
}

// ---- Low-level decoder -----------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TtLtcFrame {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: u8,
    pub locked: u8,
    pub estimated_fps: i16,
    pub sample_index: i64,
}

pub struct TtDecoder {
    decoder: Decoder,
    frames: Vec<TtLtcFrame>,
}

impl TtDecoder {
    fn store(&mut self, frames: Vec<DecodedFrame>) -> i32 {
        self.frames = frames
            .into_iter()
            .map(|f| TtLtcFrame {
                hours: f.frame.label.hours,
                minutes: f.frame.label.minutes,
                seconds: f.frame.label.seconds,
                frames: f.frame.label.frames,
                drop_frame: u8::from(f.frame.label.drop_frame),
                locked: u8::from(f.locked),
                estimated_fps: f.estimated_fps.map_or(-1, |v| v as i16),
                sample_index: f.frame.sample_index as i64,
            })
            .collect();
        self.frames.len() as i32
    }
}

#[no_mangle]
pub extern "C" fn tt_decoder_new(sample_rate: u32) -> *mut TtDecoder {
    guard(std::ptr::null_mut(), || {
        if sample_rate == 0 {
            return std::ptr::null_mut();
        }
        Box::into_raw(Box::new(TtDecoder {
            decoder: Decoder::new(sample_rate),
            frames: Vec::new(),
        }))
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_free(decoder: *mut TtDecoder) {
    if !decoder.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(decoder))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_push(
    decoder: *mut TtDecoder,
    samples: *const i16,
    len: usize,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let (Some(d), Some(pcm)) = (decoder.as_mut(), slice(samples, len)) else {
            return TT_ERR_NULL;
        };
        let first = d.decoder.sample_index;
        let frames = d.decoder.feed_at(first, pcm);
        d.store(frames)
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_push_bytes(
    decoder: *mut TtDecoder,
    bytes: *const u8,
    len: usize,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let (Some(d), Some(data)) = (decoder.as_mut(), slice(bytes, len)) else {
            return TT_ERR_NULL;
        };
        let frames = d.decoder.feed_bytes(data);
        d.store(frames)
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_frames(decoder: *const TtDecoder) -> *const TtLtcFrame {
    match decoder.as_ref() {
        Some(d) if !d.frames.is_empty() => d.frames.as_ptr(),
        _ => std::ptr::null(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_locked(decoder: *const TtDecoder) -> i32 {
    decoder
        .as_ref()
        .map_or(TT_ERR_NULL, |d| i32::from(d.decoder.locked()))
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_estimated_fps(decoder: *const TtDecoder) -> i32 {
    decoder
        .as_ref()
        .and_then(|d| d.decoder.estimated_fps())
        .map_or(-1, |f| f as i32)
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_signal_level(decoder: *const TtDecoder) -> f64 {
    decoder.as_ref().map_or(0., |d| d.decoder.signal_level())
}

#[no_mangle]
pub unsafe extern "C" fn tt_decoder_sample_index(decoder: *const TtDecoder) -> i64 {
    decoder
        .as_ref()
        .map_or(0, |d| d.decoder.sample_index as i64)
}

// ---- Clock -----------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TtClockConfig {
    pub max_age_seconds: f64,
    pub stale_after_ns: i64,
    pub max_points: u32,
    pub min_points_for_fit: u32,
    pub min_points_usable: u32,
    pub reserved: u32,
    pub min_span_seconds: f64,
    pub max_residual_frames: f64,
    pub jump_reset_frames: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TtClockState {
    pub has_fit: i32,
    pub usable: i32,
    pub a: f64,
    pub b: f64,
    pub residual: f64,
    pub nominal_fps: i32,
    pub observation_count: u32,
    pub window_span_seconds: f64,
}

/// The clock plus the host-time mapping of the Dart `LtcClockModel`.
pub struct TtClock {
    clock: Clock,
    host_offset_us: Option<f64>,
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_default_config(out: *mut TtClockConfig) {
    if let Some(out) = out.as_mut() {
        let c = ClockConfig::default();
        *out = TtClockConfig {
            max_age_seconds: c.max_age_seconds,
            stale_after_ns: c.stale_after_ns as i64,
            max_points: c.max_points as u32,
            min_points_for_fit: c.min_points_for_fit as u32,
            min_points_usable: c.min_points_usable as u32,
            reserved: 0,
            min_span_seconds: c.min_span_seconds,
            max_residual_frames: c.max_residual_frames,
            jump_reset_frames: c.jump_reset_frames,
        };
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_new(config: *const TtClockConfig) -> *mut TtClock {
    guard(std::ptr::null_mut(), || {
        let config = match config.as_ref() {
            None => ClockConfig::default(),
            Some(c) => ClockConfig {
                max_age_seconds: c.max_age_seconds,
                stale_after_ns: c.stale_after_ns.max(0) as u64,
                max_points: c.max_points as usize,
                min_points_for_fit: c.min_points_for_fit as usize,
                min_points_usable: c.min_points_usable as usize,
                min_span_seconds: c.min_span_seconds,
                max_residual_frames: c.max_residual_frames,
                jump_reset_frames: c.jump_reset_frames,
            },
        };
        if config.min_points_for_fit < 2
            || config.max_points < config.min_points_for_fit
            || config.min_points_usable < config.min_points_for_fit
        {
            return std::ptr::null_mut();
        }
        Box::into_raw(Box::new(TtClock {
            clock: Clock::new(config),
            host_offset_us: None,
        }))
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_free(clock: *mut TtClock) {
    if !clock.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(clock))));
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_add_observation(
    clock: *mut TtClock,
    sample_time: f64,
    tc: f64,
    frames_per_day: i64,
    now_ns: i64,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(c) = clock.as_mut() {
            let day = (frames_per_day > 0).then_some(frames_per_day);
            c.clock
                .add_observation(sample_time, tc, day, now_ns.max(0) as u64);
        }
    }));
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_note_host_time(
    clock: *mut TtClock,
    sample_time: f64,
    host_micros: i64,
) {
    if let Some(c) = clock.as_mut() {
        let offset = host_micros as f64 - sample_time * 1e6;
        c.host_offset_us = Some(match c.host_offset_us {
            None => offset,
            Some(previous) => previous + 0.2 * (offset - previous),
        });
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_state(
    clock: *const TtClock,
    now_ns: i64,
    out: *mut TtClockState,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let (Some(c), Some(out)) = (clock.as_ref(), out.as_mut()) else {
            return;
        };
        // As the reference `LtcClockModel`: the fit counts only while fresh
        // (not stale at `now_ns`) and backed by enough observations.
        let snap = c.clock.snapshot(now_ns.max(0) as u64);
        *out = TtClockState {
            has_fit: i32::from(snap.b.is_some()),
            usable: i32::from(snap.usable),
            a: snap.a.unwrap_or(0.),
            b: snap.b.unwrap_or(0.),
            residual: snap.residual.unwrap_or(0.),
            nominal_fps: snap.nominal_fps.map_or(-1, |f| f as i32),
            observation_count: c.clock.observation_count() as u32,
            window_span_seconds: c.clock.window_span_seconds(),
        };
    }));
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_tc_at_sample_time(
    clock: *const TtClock,
    sample_time: f64,
    now_ns: i64,
    out: *mut f64,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let (Some(c), false) = (clock.as_ref(), out.is_null()) else {
            return TT_ERR_NULL;
        };
        match c.clock.tc_at_sample_time(sample_time, now_ns.max(0) as u64) {
            Some(v) => {
                *out = v;
                1
            }
            None => 0,
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_tc_at_host_micros(
    clock: *const TtClock,
    host_micros: i64,
    now_ns: i64,
    out: *mut f64,
) -> i32 {
    guard(TT_ERR_PANIC, || {
        let (Some(c), false) = (clock.as_ref(), out.is_null()) else {
            return TT_ERR_NULL;
        };
        let Some(offset) = c.host_offset_us else {
            return 0;
        };
        let t = (host_micros as f64 - offset) / 1e6;
        match c.clock.tc_at_sample_time(t, now_ns.max(0) as u64) {
            Some(v) => {
                *out = v;
                1
            }
            None => 0,
        }
    })
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_reset(clock: *mut TtClock) {
    if let Some(c) = clock.as_mut() {
        c.clock.reset();
        c.host_offset_us = None;
    }
}

#[no_mangle]
pub unsafe extern "C" fn tt_clock_on_signal_lost(clock: *mut TtClock) {
    tt_clock_reset(clock);
}

// ---- Encoder ---------------------------------------------------------------

#[repr(C)]
#[derive(Debug)]
pub struct TtSamples {
    pub data: *mut i16,
    pub len: usize,
}

#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub extern "C" fn tt_encode_ltc(
    sample_rate: u32,
    fps: u32,
    drop_frame: i32,
    hours: u8,
    minutes: u8,
    seconds: u8,
    frames: u8,
    frame_count: u32,
    amplitude: i16,
    preamble_bit: u8,
) -> TtSamples {
    let empty = TtSamples {
        data: std::ptr::null_mut(),
        len: 0,
    };
    guard(
        TtSamples {
            data: std::ptr::null_mut(),
            len: 0,
        },
        || {
            if sample_rate == 0
                || !valid_fps(fps)
                || preamble_bit > 1
                || frame_count > 24 * 3600 * 30
            {
                return empty;
            }
            let drop_frame = drop_frame != 0;
            let start = Label {
                hours,
                minutes,
                seconds,
                frames,
                drop_frame,
            }
            .frame_index(fps);
            let pcm = generate(SynthConfig {
                sample_rate,
                rate: f64::from(fps),
                nominal_fps: fps,
                drop_frame,
                start_index: start,
                frame_count: frame_count as usize,
                amplitude: f64::from(amplitude),
                rise_time_us: 0.,
                preamble_bit,
                ..Default::default()
            })
            .pcm
            .into_boxed_slice();
            let len = pcm.len();
            TtSamples {
                data: Box::into_raw(pcm) as *mut i16,
                len,
            }
        },
    )
}

#[no_mangle]
pub unsafe extern "C" fn tt_samples_free(samples: TtSamples) {
    if !samples.data.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                samples.data,
                samples.len,
            )))
        }));
    }
}
