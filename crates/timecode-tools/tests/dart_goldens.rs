//! Parity tests against the Dart reference implementation.
//!
//! `goldens/decoder-clock/*.json` (at the repository root) were produced by
//! running the Dart LTC decoder and
//! clock (the reference implementation, published as `timecode_tools` on pub.dev)
//! over 8 decoder scenarios and 8 clock scenarios. This file replays the same
//! inputs through `decoder::Decoder` and `clock::Clock` and requires the same
//! outputs: integers and strings exactly, f64 within 1e-12 relative. That bar
//! is meaningful because both implementations use the same summation order.
//!
//! For encoded fixtures the PCM is regenerated with `synth::generate` and
//! checked against the fixture's SHA-256 (and raw samples when embedded), so
//! the generator is verified byte for byte as well. SHA-256 is implemented
//! here (checked against the FIPS 180-4 test vectors) to keep the test
//! dependencies to `serde_json` alone.
//!
//! Only `Decoder` and `Clock` are compared. `TcModel` and `Session` add
//! deliberate behavior on top of the reference (see the crate docs), so they
//! are covered by `tests/synthetic.rs` instead. For the same reason the
//! 16-channel fixture is checked as a plain decode of the LTC channel:
//! de-interleaving is lossless, so it cannot change the decoded result.
#![forbid(unsafe_code)]

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use timecode_tools::{
    clock::{Clock, ClockConfig},
    decoder::{DecodedFrame, Decoder},
    label::Label,
    synth::{generate, SynthConfig},
};

// ---------------------------------------------------------------------
// Fixture loading
// ---------------------------------------------------------------------

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goldens/decoder-clock"
    ))
    .join(format!("{name}.json"))
}

/// Parses a fixture with `serde_json`. The `float_roundtrip` feature
/// (Cargo.toml) is required: without it the float parser is not always
/// correctly rounding (for example "100000.06666666667" parses one ULP high),
/// which would show up as false mismatches. Every f64 read from a fixture
/// depends on this.
fn load_fixture(name: &str) -> Value {
    let path = fixture_path(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing fixture {name}: {e}"))
}

// --- serde_json::Value field accessors (panic with context on shape
// mismatch, since a malformed fixture is a hard test-infrastructure bug,
// not a soft failure worth propagating as a Result). ---------------------

fn as_f64(v: &Value) -> f64 {
    v.as_f64()
        .unwrap_or_else(|| panic!("expected f64, got {v:?}"))
}
fn as_opt_f64(v: &Value) -> Option<f64> {
    if v.is_null() {
        None
    } else {
        Some(as_f64(v))
    }
}
fn as_u64(v: &Value) -> u64 {
    v.as_u64()
        .unwrap_or_else(|| panic!("expected u64, got {v:?}"))
}
fn as_i64(v: &Value) -> i64 {
    v.as_i64()
        .unwrap_or_else(|| panic!("expected i64, got {v:?}"))
}
fn as_opt_i64(v: &Value) -> Option<i64> {
    if v.is_null() {
        None
    } else {
        Some(as_i64(v))
    }
}
fn as_bool(v: &Value) -> bool {
    v.as_bool()
        .unwrap_or_else(|| panic!("expected bool, got {v:?}"))
}
fn as_str(v: &Value) -> &str {
    v.as_str()
        .unwrap_or_else(|| panic!("expected str, got {v:?}"))
}

// ---------------------------------------------------------------------
// Float comparison: integers exactly, f64 within 1e-12 relative. The small absolute floor keeps the check meaningful for expected
// values near zero (a jitter probe, a fresh residual) without weakening it
// for anything of ordinary magnitude.
// ---------------------------------------------------------------------

fn assert_close(actual: f64, expected: f64, context: &str) {
    let tol = 1e-12 * expected.abs().max(1e-9);
    assert!(
        (actual - expected).abs() <= tol,
        "{context}: {actual} != {expected} (tol {tol})"
    );
}

fn assert_close_opt(actual: Option<f64>, expected: Option<f64>, context: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(a), Some(e)) => assert_close(a, e, context),
        _ => panic!("{context}: {actual:?} != {expected:?} (exactly one side is None)"),
    }
}

// ---------------------------------------------------------------------
// Hand-rolled SHA-256 (FIPS 180-4). See this file's top doc comment for
// why this is hand-rolled rather than a `sha2` dev-dependency.
// ---------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(message: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (message.len() as u64) * 8;
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let o = i * 4;
            *word = u32::from_be_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn sha256_hex(message: &[u8]) -> String {
    sha256(message).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn sha256_self_check() {
    assert_eq!(
        sha256_hex(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// ---------------------------------------------------------------------
// Decoder fixtures
// ---------------------------------------------------------------------

fn pcm_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn read_pcm_i16(fixture: &Value) -> Option<Vec<i16>> {
    fixture.get("pcm_i16").map(|v| {
        v.as_array()
            .unwrap_or_else(|| panic!("pcm_i16 must be a JSON array"))
            .iter()
            .map(|x| as_i64(x) as i16)
            .collect()
    })
}

fn start_index_from_encoder(enc: &Value, fps: u32, drop_frame: bool) -> i64 {
    Label {
        hours: as_u64(&enc["start_hours"]) as u8,
        minutes: as_u64(&enc["start_minutes"]) as u8,
        seconds: as_u64(&enc["start_seconds"]) as u8,
        frames: as_u64(&enc["start_frames"]) as u8,
        drop_frame,
    }
    .frame_index(fps)
}

/// Builds the `SynthConfig` that matches the Dart square-wave test encoder:
/// `rise_time_us: 0.` reproduces its flat (unramped) cell fill, and every
/// other field stays at `SynthConfig::default()` (no ppm, varispeed, noise or
/// dropouts), which the Dart encoder does not have.
fn build_synth_config(fixture: &Value) -> SynthConfig {
    let sample_rate = as_u64(&fixture["sample_rate"]) as u32;
    let enc = &fixture["encoder"];
    let fps = as_u64(&enc["fps"]) as u32;
    let drop_frame = as_bool(&enc["drop_frame"]);
    SynthConfig {
        sample_rate,
        rate: f64::from(fps),
        nominal_fps: fps,
        drop_frame,
        start_index: start_index_from_encoder(enc, fps, drop_frame),
        frame_count: as_u64(&enc["frame_count"]) as usize,
        amplitude: as_f64(&enc["amplitude"]),
        rise_time_us: 0.,
        preamble_bit: as_u64(&enc["preamble_bit"]) as u8,
        ..Default::default()
    }
}

fn maybe_invert(pcm: Vec<i16>, invert: bool) -> Vec<i16> {
    if invert {
        pcm.iter().map(|s| s.saturating_neg()).collect()
    } else {
        pcm
    }
}

/// Asserts every decoded frame — and the decoder's own final `locked` /
/// `estimated_fps` state — matches the fixture exactly (integers/strings)
/// or within 1e-12 relative (there are no floats among these fields today,
/// but routing through `assert_close` costs nothing and stays correct if
/// that changes).
fn assert_frames_match(fixture: &Value, decoded: &[DecodedFrame], decoder: &Decoder) {
    let name = as_str(&fixture["name"]);
    let expected_frames = fixture["frames"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: fixture has no frames array"));
    assert_eq!(decoded.len(), expected_frames.len(), "{name}: frame count");

    for (i, (got, want)) in decoded.iter().zip(expected_frames).enumerate() {
        let ctx = format!("{name} frame {i}");
        assert_eq!(
            got.frame.label.to_string(),
            as_str(&want["timecode"]),
            "{ctx}: timecode"
        );
        assert_eq!(
            got.frame.sample_index,
            as_u64(&want["sample_index"]),
            "{ctx}: sample_index"
        );
        assert_eq!(got.locked, as_bool(&want["locked"]), "{ctx}: locked");
        assert_eq!(
            got.estimated_fps.map(i64::from),
            as_opt_i64(&want["estimated_fps"]),
            "{ctx}: estimated_fps"
        );
    }

    assert_eq!(
        decoder.locked(),
        as_bool(&fixture["final_locked"]),
        "{name}: final_locked"
    );
    assert_eq!(
        decoder.estimated_fps().map(i64::from),
        as_opt_i64(&fixture["final_estimated_fps"]),
        "{name}: final_estimated_fps"
    );
}

/// Verifies a `"kind": "encoded"` fixture: regenerates the PCM from the
/// fixture's own `encoder` parameters via the Rust `synth` module (proving
/// the encoder port byte-identical against `pcm_sha256`, and against the
/// embedded `pcm_i16` samples when present), then decodes that PCM and
/// compares every frame.
fn verify_encoded_decoder_fixture(fixture_name: &str) {
    let fixture = load_fixture(fixture_name);
    let sample_rate = as_u64(&fixture["sample_rate"]) as u32;
    let invert = fixture["encoder"]["invert"].as_bool().unwrap_or(false);

    let signal = generate(build_synth_config(&fixture));
    let pcm = maybe_invert(signal.pcm, invert);
    let bytes = pcm_to_bytes(&pcm);

    let hash = sha256_hex(&bytes);
    assert_eq!(
        hash,
        as_str(&fixture["pcm_sha256"]),
        "{fixture_name}: pcm_sha256 — Rust synth is not byte-identical to \
         Dart's encoder for this fixture's parameters"
    );
    if let Some(expected_pcm) = read_pcm_i16(&fixture) {
        assert_eq!(
            pcm, expected_pcm,
            "{fixture_name}: pcm_i16 sample-level mismatch"
        );
    }

    let mut decoder = Decoder::new(sample_rate);
    let decoded = decoder.feed_bytes(&bytes);
    assert_frames_match(&fixture, &decoded, &decoder);
}

/// Verifies a `"kind": "silence"` or `"kind": "white_noise"` fixture. This
/// PCM is not regenerated by `synth` (silence is trivial; white noise comes
/// from Dart's `Random`), so it is read from the fixture itself
/// (`sample_count` zeros for silence, `pcm_i16` for noise).
fn verify_raw_decoder_fixture(fixture_name: &str) {
    let fixture = load_fixture(fixture_name);
    let sample_rate = as_u64(&fixture["sample_rate"]) as u32;
    let kind = as_str(&fixture["kind"]);
    let pcm: Vec<i16> = match kind {
        "silence" => vec![0i16; as_u64(&fixture["sample_count"]) as usize],
        "white_noise" => read_pcm_i16(&fixture)
            .unwrap_or_else(|| panic!("{fixture_name}: white_noise fixture has no pcm_i16")),
        other => panic!("{fixture_name}: unexpected raw fixture kind {other:?}"),
    };
    let bytes = pcm_to_bytes(&pcm);

    let hash = sha256_hex(&bytes);
    assert_eq!(
        hash,
        as_str(&fixture["pcm_sha256"]),
        "{fixture_name}: pcm_sha256"
    );

    let mut decoder = Decoder::new(sample_rate);
    let decoded = decoder.feed_bytes(&bytes);
    assert_frames_match(&fixture, &decoded, &decoder);
}

#[test]
fn decoder_ascending_25fps_uneven_chunks() {
    verify_encoded_decoder_fixture("decoder_ascending_25fps_uneven_chunks");
}

#[test]
fn decoder_inverted_polarity() {
    verify_encoded_decoder_fixture("decoder_inverted_polarity");
}

#[test]
fn decoder_silence() {
    verify_raw_decoder_fixture("decoder_silence");
}

#[test]
fn decoder_white_noise() {
    verify_raw_decoder_fixture("decoder_white_noise");
}

#[test]
fn decoder_odd_length_byte_carry() {
    verify_encoded_decoder_fixture("decoder_odd_length_byte_carry");
}

#[test]
fn decoder_df_boundary() {
    verify_encoded_decoder_fixture("decoder_df_boundary");
}

#[test]
fn decoder_full_cell_bootstrap() {
    verify_encoded_decoder_fixture("decoder_full_cell_bootstrap");
}

/// 16-channel capture with music-like audio on CH 1 and LTC on CH 2.
/// De-interleaving is lossless, so decoding CH 2 depends only on CH 2's own
/// samples: the fixture's `pcm_sha256` and `frames` are reproduced here from
/// the LTC channel's encoder parameters alone. `channel_count` and the channel
/// indices in the fixture document the scenario; they are not inputs here.
/// Channel election is covered by the `Session` tests instead.
#[test]
fn decoder_16ch_interleave_music_ch1() {
    verify_encoded_decoder_fixture("decoder_16ch_interleave_music_ch1");
}

// ---------------------------------------------------------------------
// Clock fixtures
// ---------------------------------------------------------------------

fn clock_config_from_json(cfg: &Value) -> ClockConfig {
    ClockConfig {
        max_age_seconds: as_f64(&cfg["max_age_seconds"]),
        stale_after_ns: as_u64(&cfg["stale_after_ns"]),
        max_points: as_u64(&cfg["max_points"]) as usize,
        min_points_for_fit: as_u64(&cfg["min_points_for_fit"]) as usize,
        min_points_usable: as_u64(&cfg["min_points_usable"]) as usize,
        min_span_seconds: as_f64(&cfg["min_span_seconds"]),
        max_residual_frames: as_f64(&cfg["max_residual_frames"]),
        jump_reset_frames: as_f64(&cfg["jump_reset_frames"]),
    }
}

/// Replays a clock fixture's `steps` script against a fresh `Clock`.
///
/// The fit intercept `a` is compared only when the fixture recorded a
/// non-null value. The Dart clock exposes the intercept only through its
/// `usable`-gated `tcAtSampleTime`, while the rate (`b`) and residual are
/// available whenever a fit exists. So `"a": null` with `"usable": false`
/// means "not observable in Dart", not "expected None"; `snap.a` legitimately
/// holds a value on the Rust side in that case.
fn verify_clock_fixture(fixture_name: &str) {
    let fixture = load_fixture(fixture_name);
    let name = as_str(&fixture["name"]).to_string();
    let mut clock = Clock::new(clock_config_from_json(&fixture["config"]));

    let steps = fixture["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}: fixture has no steps array"));
    for (i, step) in steps.iter().enumerate() {
        let op = as_str(&step["op"]);
        let ctx = format!("{name} step {i} ({op})");
        match op {
            "observe" => {
                let t = as_f64(&step["t"]);
                let tc = as_f64(&step["tc"]);
                let day = as_opt_i64(&step["frames_per_day"]);
                let now_ns = as_u64(&step["now_ns"]);
                clock.add_observation(t, tc, day, now_ns);
            }
            "snapshot" => {
                let now_ns = as_u64(&step["now_ns"]);
                let snap = clock.snapshot(now_ns);
                assert_eq!(snap.usable, as_bool(&step["usable"]), "{ctx}: usable");
                assert_eq!(
                    snap.nominal_fps.map(i64::from),
                    as_opt_i64(&step["nominal_fps"]),
                    "{ctx}: nominal_fps"
                );
                assert_close_opt(snap.b, as_opt_f64(&step["b"]), &format!("{ctx}: b"));
                assert_close_opt(
                    snap.residual,
                    as_opt_f64(&step["residual"]),
                    &format!("{ctx}: residual"),
                );
                if let Some(expected_a) = as_opt_f64(&step["a"]) {
                    let actual_a = snap
                        .a
                        .unwrap_or_else(|| panic!("{ctx}: a — expected Some, Rust had None"));
                    assert_close(actual_a, expected_a, &format!("{ctx}: a"));
                }
            }
            "probe" => {
                let t = as_f64(&step["t"]);
                let now_ns = as_u64(&step["now_ns"]);
                let value = clock.tc_at_sample_time(t, now_ns);
                assert_close_opt(
                    value,
                    as_opt_f64(&step["value"]),
                    &format!("{ctx}: probe value"),
                );
            }
            other => panic!("{ctx}: unknown step op {other:?}"),
        }
    }
}

#[test]
fn clock_exact_30fps() {
    verify_clock_fixture("clock_exact_30fps");
}

#[test]
fn clock_fractional_23976() {
    verify_clock_fixture("clock_fractional_23976");
}

#[test]
fn clock_jitter_20ms() {
    verify_clock_fixture("clock_jitter_20ms");
}

#[test]
fn clock_varispeed_half_percent() {
    verify_clock_fixture("clock_varispeed_half_percent");
}

#[test]
fn clock_jam_reset() {
    verify_clock_fixture("clock_jam_reset");
}

#[test]
fn clock_staleness_scripted() {
    verify_clock_fixture("clock_staleness_scripted");
}

#[test]
fn clock_midnight_unwrap() {
    verify_clock_fixture("clock_midnight_unwrap");
}

#[test]
fn clock_long_session_100000s() {
    verify_clock_fixture("clock_long_session_100000s");
}
