//! Prints the Rust layout of the C structs (compared with the C compiler's
//! by scripts/check-c-abi.sh) and checks the sizes.
use std::mem::{offset_of, size_of};
use timecode_tools_ffi::*;

#[test]
fn layout() {
    let lines = [
        format!(
            "layout TtTimecode {} {} {} {}",
            size_of::<TtTimecode>(),
            offset_of!(TtTimecode, timestamp_ns),
            offset_of!(TtTimecode, sample_index),
            offset_of!(TtTimecode, channel)
        ),
        format!(
            "layout TtEvent {} {}",
            size_of::<TtEvent>(),
            offset_of!(TtEvent, timecode)
        ),
        format!(
            "layout TtReceiverOptions {} {}",
            size_of::<TtReceiverOptions>(),
            offset_of!(TtReceiverOptions, offset_frames)
        ),
        format!(
            "layout TtLtcFrame {} {}",
            size_of::<TtLtcFrame>(),
            offset_of!(TtLtcFrame, sample_index)
        ),
        format!(
            "layout TtClockConfig {} {}",
            size_of::<TtClockConfig>(),
            offset_of!(TtClockConfig, min_span_seconds)
        ),
        format!(
            "layout TtClockState {} {}",
            size_of::<TtClockState>(),
            offset_of!(TtClockState, window_span_seconds)
        ),
    ];
    if let Ok(path) = std::env::var("TT_LAYOUT_OUT") {
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }
    assert_eq!(size_of::<TtTimecode>(), 32);
    assert_eq!(size_of::<TtEvent>(), 48);
    assert_eq!(size_of::<TtLtcFrame>(), 16);
}
