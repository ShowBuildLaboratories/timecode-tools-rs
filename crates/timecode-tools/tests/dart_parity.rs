//! Hand-written checks that mirror the Dart reference test suite: decoder,
//! timecode arithmetic, clock and channel selection.
use timecode_tools::{
    clock::{Clock, ClockConfig},
    decoder::Decoder,
    label::{frames_per_day, Label},
    session::{Channel, Session, SessionError},
    synth::{generate, SynthConfig},
};
fn close(a: f64, b: f64, t: f64) {
    assert!((a - b).abs() < t, "{a} != {b} (tol {t})");
}
fn sequence(config: SynthConfig) -> Vec<timecode_tools::decoder::DecodedFrame> {
    let s = generate(config);
    let mut d = Decoder::new(s.config.sample_rate);
    s.bytes()
        .chunks(137)
        .flat_map(|b| d.feed_bytes(b))
        .collect()
}
fn square() -> SynthConfig {
    SynthConfig {
        rise_time_us: 0.,
        frame_count: 10,
        ..Default::default()
    }
}
#[test]
fn decodes_ascending_25fps_uneven_chunks() {
    let c = SynthConfig {
        start_index: 93079,
        ..square()
    };
    let out = sequence(c);
    assert_eq!(out.len(), 10);
    for (i, f) in out.iter().enumerate() {
        assert_eq!(f.frame.label.frame_index(25), 93079 + i as i64);
    }
    assert!(out[9].locked);
    assert_eq!(out[9].estimated_fps, Some(25));
}
#[test]
fn pure_silence_never_emits() {
    assert!(Decoder::new(48000).feed_bytes(&vec![0; 96000]).is_empty());
}
#[test]
fn white_noise_never_emits() {
    let mut n = 7_u64;
    let pcm: Vec<i16> = (0..48000)
        .map(|_| {
            n ^= n << 13;
            n ^= n >> 7;
            n ^= n << 17;
            n as i16
        })
        .collect();
    assert!(Decoder::new(48000).feed_at(0, &pcm).is_empty());
}
#[test]
fn inverted_signal_polarity() {
    let s = generate(square());
    let p: Vec<i16> = s.pcm.iter().map(|s| -s).collect();
    let a = Decoder::new(48000).feed_at(0, &s.pcm);
    let b = Decoder::new(48000).feed_at(0, &p);
    assert_eq!(
        a.iter().map(|x| x.frame).collect::<Vec<_>>(),
        b.iter().map(|x| x.frame).collect::<Vec<_>>()
    );
}
#[test]
fn leftover_byte_odd_split() {
    let s = generate(square());
    let bytes = s.bytes();
    let mut d = Decoder::new(48000);
    let mut out = d.feed_bytes(&bytes[..13]);
    out.extend(d.feed_bytes(&[]));
    out.extend(d.feed_bytes(&bytes[13..]));
    assert_eq!(out.len(), 10);
    assert_eq!(out[0].frame.label.frames, 0);
}
#[test]
fn sample_index_monotonic_across_chunks() {
    let out = sequence(square());
    for pair in out.windows(2) {
        assert_eq!(
            pair[1].frame.sample_index - pair[0].frame.sample_index,
            1920
        );
    }
    assert_eq!(out[0].frame.sample_index, 2113);
}
#[test]
fn drop_frame_lock_across_non_tenth_minute() {
    let out = sequence(SynthConfig {
        rate: 30.,
        nominal_fps: 30,
        drop_frame: true,
        start_index: 58 * 30,
        frame_count: 90,
        ..square()
    });
    assert_eq!(out.len(), 90);
    assert!(out.iter().skip(3).all(|f| f.locked));
    let at = out.iter().position(|f| f.frame.label.minutes == 1).unwrap();
    assert_eq!(out[at].frame.label.frames, 2);
    assert_eq!(out[at - 1].frame.label.frames, 29);
}
#[test]
fn two_hypothesis_full_cell_bootstrap() {
    let out = sequence(SynthConfig {
        preamble_bit: 0,
        start_index: 276131,
        ..square()
    });
    assert_eq!(out.len(), 10);
    assert!(out[9].locked);
}
#[test]
fn round_trips_every_frame_24h_df() {
    for i in 0..frames_per_day(30, true) {
        assert_eq!(Label::from_index(i, 30, true).frame_index(30), i);
    }
}
#[test]
fn round_trips_sampled_24_25_30_ndf() {
    for fps in [24, 25, 30] {
        let day = frames_per_day(fps, false);
        for i in (-day..day * 2).step_by(997) {
            assert_eq!(
                Label::from_index(i, fps, false).frame_index(fps),
                i.rem_euclid(day)
            );
        }
    }
}
#[test]
fn textbook_drop_frame_identities() {
    assert_eq!(Label::from_index(1800, 30, true).to_string(), "00:01:00;02");
    assert_eq!(
        Label::from_index(17982, 30, true).to_string(),
        "00:10:00;00"
    );
}
fn clock(rate: f64, start: f64) -> Clock {
    let mut c = Clock::default();
    for i in 0..40 {
        c.add_observation(start + i as f64 / rate, i as f64, None, 0);
    }
    c
}
#[test]
fn exact_30fps_subframe_rate_nominal() {
    let c = clock(30., 0.);
    let s = c.snapshot(0);
    assert!(s.usable);
    close(s.b.unwrap(), 30., 0.01);
    assert_eq!(s.nominal_fps, Some(30));
    close(c.tc_at_sample_time(20.5 / 30., 0).unwrap(), 20.5, 0.05);
}
#[test]
fn fractional_23976_distinguished_from_24() {
    let c = clock(24000. / 1001., 0.);
    close(c.snapshot(0).b.unwrap(), 24000. / 1001., 0.01);
    assert_eq!(c.snapshot(0).nominal_fps, Some(24));
}
#[test]
fn jittered_sample_times_20ms() {
    let mut c = Clock::default();
    let jitter = [-20., 12., -8., 20., -15., 5., -20., 18., -3., 20.];
    for i in 0..40 {
        c.add_observation(i as f64 / 30. + jitter[i % 10] / 1000., i as f64, None, 0);
    }
    assert!(c.snapshot(0).usable);
    assert!(c.snapshot(0).residual.unwrap() < 0.5);
    close(c.snapshot(0).b.unwrap(), 30., 0.5);
    assert_eq!(c.tc_at_sample_time(1., 0), c.tc_at_sample_time(1., 0));
}
#[test]
fn varispeed_half_percent() {
    close(clock(30.15, 0.).snapshot(0).b.unwrap(), 30.15, 0.02);
}
#[test]
fn jump_over_five_resets_window() {
    let mut c = clock(30., 0.);
    c.add_observation(40. / 30., 500., None, 0);
    assert_eq!(c.observation_count(), 1);
    assert!(!c.snapshot(0).usable);
    for i in 1..20 {
        c.add_observation((40 + i) as f64 / 30., 500. + i as f64, None, 0);
    }
    close(c.tc_at_sample_time(59. / 30., 0).unwrap(), 519., 0.05);
}
#[test]
fn stale_observations_unusable() {
    let c = clock(30., 0.);
    assert!(!c.snapshot(4_000_000_001).usable);
    assert!(c.snapshot(4_000_000_001).b.is_none());
}
#[test]
fn staleness_immune_to_wall_clock_jump() {
    let mut c = Clock::new(ClockConfig {
        stale_after_ns: 50_000_000,
        ..Default::default()
    });
    for i in 0..20 {
        c.add_observation(i as f64 / 30., i as f64, None, 100_000_000_000_000);
    }
    assert!(c.snapshot(100_000_010_000_000).usable);
    assert!(!c.snapshot(100_000_200_000_000).usable);
}
#[test]
fn signal_lost_clears_usability() {
    let mut c = clock(30., 0.);
    c.on_signal_lost();
    assert!(!c.snapshot(0).usable);
    assert_eq!(c.observation_count(), 0);
}
#[test]
fn non_drop_naive_math() {
    assert_eq!(
        Label {
            hours: 1,
            minutes: 0,
            seconds: 0,
            frames: 0,
            drop_frame: false
        }
        .frame_index(30),
        108000
    );
}
#[test]
fn drop_frame_one_hour() {
    assert_eq!(
        Label {
            hours: 1,
            minutes: 0,
            seconds: 0,
            frames: 0,
            drop_frame: true
        }
        .frame_index(30),
        107892
    );
}
#[test]
fn drop_frame_zero() {
    assert_eq!(Label::from_index(0, 30, true).frame_index(30), 0);
}
#[test]
fn frames_per_day_last_valid() {
    for df in [false, true] {
        assert_eq!(
            Label {
                hours: 23,
                minutes: 59,
                seconds: 59,
                frames: 29,
                drop_frame: df
            }
            .frame_index(30)
                + 1,
            frames_per_day(30, df)
        );
    }
}
#[test]
fn midnight_unwrap() {
    let day = frames_per_day(30, false);
    let mut c = Clock::default();
    for i in 0..40 {
        c.add_observation(i as f64 / 30., ((day - 20 + i) % day) as f64, Some(day), 0);
    }
    close(
        c.tc_at_sample_time(20. / 30., 0).unwrap(),
        day as f64,
        0.001,
    );
    assert_eq!(c.observation_count(), 40);
}
#[test]
fn long_session_centered_least_squares() {
    let c = clock(30., 100000.);
    close(c.snapshot(0).b.unwrap(), 30., 1e-6);
    assert!(c.snapshot(0).residual.unwrap() < 1e-6);
}
fn channels(count: usize, live: usize, selection: Channel, rate: u32) -> Session {
    let s = generate(SynthConfig {
        sample_rate: rate,
        ..square()
    });
    let mut session = Session::new(rate, count, selection);
    let pcm: Vec<i16> = s
        .pcm
        .iter()
        .flat_map(|&v| (0..count).map(move |i| if i == live { v } else { 0 }))
        .collect();
    assert!(!session.feed(0, &pcm, 0).unwrap().is_empty());
    assert_eq!(session.active_channel(), Some(live + 1));
    session
}
#[test]
fn fixed_ch5_of_16() {
    channels(16, 4, Channel::Fixed(5), 48000);
}
#[test]
fn auto_scans_all_16_and_keeps_election() {
    let mut s = channels(16, 11, Channel::Auto, 48000);
    s.tick(699_999_999);
    assert_eq!(s.active_channel(), Some(12));
}
#[test]
fn selected_channel_unavailable() {
    let mut s = Session::new(48000, 2, Channel::Fixed(5));
    assert_eq!(
        s.feed(0, &[0; 100], 0).unwrap_err(),
        SessionError::ChannelUnavailable {
            selected: 5,
            captured: 2
        }
    );
    assert_eq!(s.active_channel(), None);
}
#[test]
fn mono_fixed_ch2_unavailable() {
    assert!(Session::new(48000, 1, Channel::Fixed(2))
        .feed(0, &[0; 100], 0)
        .is_err());
}
#[test]
fn auto_mono_elects_ch1() {
    channels(1, 0, Channel::Auto, 48000);
}
#[test]
fn fixed_ch1_of_16() {
    channels(16, 0, Channel::Fixed(1), 48000);
}
#[test]
fn decodes_at_44100() {
    channels(2, 1, Channel::Fixed(2), 44100);
}
#[test]
fn live_channel_reselection() {
    let mut s = channels(16, 0, Channel::Fixed(1), 48000);
    s.select(Channel::Fixed(5));
    let signal = generate(square());
    let pcm: Vec<i16> = signal
        .pcm
        .iter()
        .flat_map(|&v| (0..16).map(move |i| if i == 4 { v } else { 0 }))
        .collect();
    assert!(!s.feed(50000, &pcm, 100_000_000).unwrap().is_empty());
    assert_eq!(s.active_channel(), Some(5));
}
#[test]
fn auto_music_ch1_ltc_ch2() {
    let signal = generate(square());
    let mut s = Session::new(48000, 16, Channel::Auto);
    let pcm: Vec<i16> = signal
        .pcm
        .iter()
        .enumerate()
        .flat_map(|(j, &v)| {
            (0..16).map(move |i| match i {
                0 => ((j as f64 * 0.03).sin() * 20000.) as i16,
                1 => v,
                _ => 0,
            })
        })
        .collect();
    assert!(!s.feed(0, &pcm, 0).unwrap().is_empty());
    assert_eq!(s.active_channel(), Some(2));
}
#[test]
fn channel_index_one_based() {
    assert_eq!(Channel::Auto.index(), None);
    assert_eq!(Channel::Fixed(2).index(), Some(2));
}
#[test]
fn channel_index_roundtrip() {
    for n in 1..=32 {
        assert_eq!(Channel::from_index(n).unwrap().index(), Some(n));
    }
    assert_eq!(Channel::from_index(33), None);
}
#[test]
fn channel_labels() {
    assert_eq!(Channel::Auto.label(), "AUTO");
    assert_eq!(Channel::Fixed(32).label(), "CH 32");
}

#[test]
fn channel_names_roundtrip() {
    for name in ["auto", "ch1", "ch2", "ch32"] {
        let c = Channel::from_name(name).unwrap();
        assert_eq!(
            c.index()
                .map_or_else(|| "auto".into(), |n| format!("ch{n}")),
            name
        );
    }
    for name in ["ch0", "ch33", "ch01", "unknown"] {
        assert!(Channel::from_name(name).is_none());
    }
    assert!(Channel::from_index(0).is_none());
}
