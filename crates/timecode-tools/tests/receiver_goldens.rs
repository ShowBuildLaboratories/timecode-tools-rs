//! Receiver parity with the Dart implementation (`timecode_tools`).
//!
//! `goldens/receiver/*.json` (at the repository root) were produced by the
//! reference Dart `LtcReceiver`
//! and `MtcReceiver` from scripted inputs (LTC from the Dart test encoder,
//! raw MIDI bytes for MTC). This test rebuilds the same inputs, runs them
//! through [`LtcReceiver`] and [`MtcReceiver`], and requires the same
//! frames (timecode, and timestamp within 3 us), the same lock / lost / jump
//! events after each input chunk, the same `framesAt` value after each
//! chunk (within 1e-6 frames), the same `waitFor` results and the same final
//! state.
use serde_json::Value;
use std::{fs, path::PathBuf};
use timecode_tools::{
    label::Label,
    ltc::{PcmChunk, Samples},
    receiver::{
        LtcReceiver, MidiChunk, MtcReceiver, ReceiverEvent, ReceiverOptions, TimecodeReceiver,
    },
    synth::{generate, SynthConfig},
    timecode::{FrameRate, Timecode},
};

fn fixtures() -> Vec<Value> {
    let dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goldens/receiver"
    ));
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(files.len() >= 7, "receiver fixtures missing");
    files
        .iter()
        .map(|p| serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

fn parse(text: &str) -> Label {
    let b = text.as_bytes();
    let n = |i: usize| text[i..i + 2].parse::<u8>().unwrap();
    Label {
        hours: n(0),
        minutes: n(3),
        seconds: n(6),
        frames: n(9),
        drop_frame: b[8] != b':',
    }
}

fn options(v: &Value) -> ReceiverOptions {
    ReceiverOptions {
        frame_rate: v["frame_rate"].as_str().and_then(FrameRate::from_name),
        offset_frames: v["offset_frames"].as_i64().unwrap_or(0),
        channel: Some(v["channel"].as_u64().unwrap_or(1) as u8),
        ..Default::default()
    }
}

fn dart_rate_label(rate: Option<FrameRate>) -> Value {
    match rate {
        None => Value::Null,
        Some(FrameRate::Fps2997Drop) => "29.97DF".into(),
        Some(r) => r.name().into(),
    }
}

/// What a receiver reported after one chunk, in the fixture's shape.
#[derive(Default)]
struct Step {
    frames: Vec<Timecode>,
    events: Vec<&'static str>,
    frames_at: Option<f64>,
}

fn replay<R: TimecodeReceiver>(fixture: &Value, rx: &mut R, chunks: Vec<R::Chunk<'_>>)
where
    R::Error: std::fmt::Debug,
{
    let name = fixture["name"].as_str().unwrap();
    let targets: Vec<(String, Label)> = fixture["wait_for"]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), parse(k)))
        .collect();
    let mut resolved: Vec<Option<(usize, Timecode)>> = vec![None; targets.len()];
    let mut steps = Vec::new();
    let expected = fixture["steps"].as_array().unwrap();
    for (i, chunk) in chunks.into_iter().enumerate() {
        let mut step = Step::default();
        for e in rx.push(chunk).unwrap() {
            for (t, (_, label)) in targets.iter().enumerate() {
                if resolved[t].is_none() && e.passes(*label) {
                    if let ReceiverEvent::Frame { timecode, .. } = e {
                        resolved[t] = Some((i, timecode));
                    }
                }
            }
            match e {
                ReceiverEvent::Lock(_) => step.events.push("lock"),
                ReceiverEvent::Lost => step.events.push("lost"),
                ReceiverEvent::Jump(_) => step.events.push("jump"),
                ReceiverEvent::Frame { timecode, .. } => step.frames.push(timecode),
            }
        }
        let at_us = expected[i]["frames_at"]["t_us"].as_u64().unwrap();
        step.frames_at = rx.frames_at(at_us * 1000);
        steps.push(step);
    }

    assert_eq!(steps.len(), expected.len(), "{name}: chunk count");
    for (i, (got, want)) in steps.iter().zip(expected).enumerate() {
        let want_events: Vec<&str> = want["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert_eq!(got.events, want_events, "{name} chunk {i}: events");
        let fa = &want["frames_at"];
        match (got.frames_at, fa["value"].as_f64()) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!(
                (g - w).abs() < 1e-6,
                "{name} chunk {i}: framesAt {g} vs {w}"
            ),
            (g, _) => panic!("{name} chunk {i}: framesAt {g:?} vs {}", fa["value"]),
        }
        let want_frames = want["frames"].as_array().unwrap();
        assert_eq!(
            got.frames.len(),
            want_frames.len(),
            "{name} chunk {i}: frame count"
        );
        for (g, w) in got.frames.iter().zip(want_frames) {
            assert_eq!(
                g.label.to_string(),
                w["tc"].as_str().unwrap(),
                "{name} chunk {i}"
            );
            check_ts(g, &w["ts_us"], &format!("{name} chunk {i} {}", g.label));
        }
    }
    for (t, (text, _)) in targets.iter().enumerate() {
        let want = &fixture["wait_for"][text];
        match (resolved[t], want.is_null()) {
            (None, true) => {}
            (Some((chunk, tc)), false) => {
                assert_eq!(
                    chunk as u64,
                    want["chunk"].as_u64().unwrap(),
                    "{name} waitFor {text}: chunk"
                );
                assert_eq!(
                    tc.label.to_string(),
                    want["tc"].as_str().unwrap(),
                    "{name} waitFor {text}"
                );
                check_ts(&tc, &want["ts_us"], &format!("{name} waitFor {text}"));
            }
            (got, _) => panic!("{name} waitFor {text}: got {got:?}, want {want}"),
        }
    }
    assert_eq!(
        rx.locked(),
        fixture["final_locked"].as_bool().unwrap(),
        "{name}: locked"
    );
    assert_eq!(
        dart_rate_label(rx.detected_rate()),
        fixture["final_detected_rate"],
        "{name}: rate"
    );
    assert_eq!(
        rx.current().map(|t| t.label.to_string()),
        fixture["final_current"].as_str().map(String::from),
        "{name}: current"
    );
}

fn check_ts(tc: &Timecode, want: &Value, context: &str) {
    match (tc.timestamp_ns, want.as_i64()) {
        (None, None) => {}
        (Some(ns), Some(us)) => {
            let diff = ns as f64 / 1e3 - us as f64;
            assert!(diff.abs() <= 3., "{context}: timestamp {ns} ns vs {us} us");
        }
        (got, _) => panic!("{context}: timestamp {got:?} vs {want}"),
    }
}

fn ltc_input(input: &Value) -> Vec<i16> {
    let sample_rate = input["sample_rate"].as_u64().unwrap() as u32;
    let rate = FrameRate::from_name(input["rate"].as_str().unwrap()).unwrap();
    let mut mono = Vec::new();
    for seg in input["segments"].as_array().unwrap() {
        if let Some(n) = seg["silence"].as_u64() {
            mono.extend(std::iter::repeat_n(0i16, n as usize));
            continue;
        }
        let start = rate
            .index_of(parse(seg["start"].as_str().unwrap()))
            .unwrap();
        // The Dart test encoder: square wave at the nominal rate.
        mono.extend(
            generate(SynthConfig {
                sample_rate,
                rate: f64::from(rate.nominal_fps()),
                nominal_fps: rate.nominal_fps(),
                drop_frame: rate.drop_frame(),
                start_index: start,
                frame_count: seg["frame_count"].as_u64().unwrap() as usize,
                rise_time_us: 0.,
                ..Default::default()
            })
            .pcm,
        );
    }
    let channels = input["channels"].as_u64().unwrap() as usize;
    let ltc = input["ltc_channel"].as_u64().unwrap() as usize;
    let out: Vec<i16> = if channels == 1 {
        mono
    } else {
        mono.iter()
            .enumerate()
            .flat_map(|(i, &s)| {
                (1..=channels).map(move |c| {
                    if c == ltc {
                        s
                    } else {
                        ((i * 37) % 4000) as i16 - 2000
                    }
                })
            })
            .collect()
    };
    assert_eq!(
        out.len() as u64,
        input["sample_count"].as_u64().unwrap(),
        "LTC input length"
    );
    out
}

#[test]
fn receivers_match_dart() {
    let mut checked = 0;
    for fixture in fixtures() {
        let input = &fixture["input"];
        let opts = options(&fixture["options"]);
        match fixture["kind"].as_str().unwrap() {
            "ltc" => {
                let pcm = ltc_input(input);
                let sample_rate = input["sample_rate"].as_u64().unwrap() as u32;
                let channels = input["channels"].as_u64().unwrap() as u16;
                let t0 = input["t0_us"].as_u64().unwrap();
                let per = input["chunk"].as_u64().unwrap() as usize * usize::from(channels);
                let chunks = pcm
                    .chunks(per)
                    .enumerate()
                    .map(|(i, block)| {
                        let first = (i * per / usize::from(channels)) as f64;
                        let us = t0 + (first * 1e6 / f64::from(sample_rate)).round() as u64;
                        PcmChunk {
                            samples: Samples::I16(block),
                            sample_rate,
                            channels,
                            timestamp_ns: Some(us * 1000),
                        }
                    })
                    .collect();
                replay(&fixture, &mut LtcReceiver::new(opts), chunks);
            }
            "mtc" => {
                let raw: Vec<(Vec<u8>, u64)> = input["chunks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| {
                        let hex = c["bytes"].as_str().unwrap();
                        let bytes = (0..hex.len())
                            .step_by(2)
                            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                            .collect();
                        (bytes, c["t_us"].as_u64().unwrap() * 1000)
                    })
                    .collect();
                let chunks = raw
                    .iter()
                    .map(|(data, ns)| MidiChunk {
                        data,
                        timestamp_ns: *ns,
                    })
                    .collect();
                replay(&fixture, &mut MtcReceiver::new(opts), chunks);
            }
            other => panic!("unknown fixture kind {other}"),
        }
        checked += 1;
    }
    assert_eq!(checked, 7);
}
