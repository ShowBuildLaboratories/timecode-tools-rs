//! End-to-end tests on generated signals: phase, rate and device-drift
//! accuracy against ground truth, states, jams, dropouts and channel election.
use timecode_tools::{
    decoder::Decoder,
    hostmap::{HostMap, IoStamp},
    model::{resolve_rate, State, TcModel},
    session::{Channel, Session},
    synth::{generate, Signal, SynthConfig},
};
fn run(signal: &Signal, block: usize, jitter: bool) -> TcModel {
    let mut model = TcModel::new(signal.config.sample_rate, signal.config.rate);
    model.set_input_state(State::NoSignal);
    let mut decoder = Decoder::new(signal.config.sample_rate);
    for (i, pcm) in signal.pcm.chunks(block).enumerate() {
        let first = i * block;
        let ns = signal.host_ns(first as u64);
        let j = if jitter {
            ((i % 2) as i64 * 2 - 1) * 50_000
        } else {
            0
        };
        model.add_stamp(IoStamp {
            first_sample: first as u64,
            host_ns: (ns as i64 + 100_000 + j) as u64,
            frames: pcm.len() as u32,
            discontinuity: false,
        });
        let now = signal.host_ns((first + pcm.len()) as u64) + 100_000;
        for event in decoder.feed_at(first as u64, pcm) {
            model.observe(event, now);
        }
        if ns > 4_000_000_000 {
            let snap = model.snapshot(now);
            {
                let tc = snap
                    .at(now)
                    .expect("settled clean signal must remain usable");
                let truth = signal.truth(now - 100_000);
                assert!(
                    (tc - truth).abs() < 0.02,
                    "phase {} rate {}",
                    tc - truth,
                    signal.config.rate
                );
                assert!(
                    (snap.b.unwrap() * 1e9 / signal.effective_rate() - 1.).abs() < 5e-6,
                    "rate {}",
                    snap.b.unwrap() * 1e9
                );
                assert!(
                    (snap.device_ppm.unwrap() - signal.config.device_ppm).abs() < 1.,
                    "ppm {:?}",
                    snap.device_ppm
                );
            }
        }
    }
    model
}
#[test]
fn all_five_rates_ground_truth_device_ppm_and_rate_after_four_seconds() {
    for (rate, fps, df) in [
        (24000. / 1001., 24, false),
        (24., 24, false),
        (25., 25, false),
        (30000. / 1001., 30, true),
        (30000. / 1001., 30, false),
        (30., 30, false),
    ] {
        for ppm in [-200., 200.] {
            for block in [32, 257, 2048] {
                let signal = generate(SynthConfig {
                    rate,
                    nominal_fps: fps,
                    drop_frame: df,
                    device_ppm: ppm,
                    frame_count: 180,
                    ..Default::default()
                });
                let mut model = run(&signal, block, true);
                assert_eq!(
                    model
                        .snapshot(signal.host_ns(signal.pcm.len() as u64) + 100_000)
                        .state,
                    State::Locked
                );
            }
        }
    }
}
#[test]
fn varispeed_and_snr_ground_truth() {
    for speed in [0.99, 1.01] {
        for snr in [30., 50.] {
            let s = generate(SynthConfig {
                varispeed: speed,
                snr_db: Some(snr),
                dc: 500.,
                frame_count: 180,
                ..Default::default()
            });
            run(&s, 256, false);
        }
    }
}
#[test]
fn midnight_end_to_end() {
    let s = generate(SynthConfig {
        start_index: 24 * 3600 * 25 - 70,
        frame_count: 180,
        ..Default::default()
    });
    let m = run(&s, 256, false);
    assert_eq!(m.epoch, 0);
}
#[test]
fn jam_confirmed_within_two_frames_and_corrupt_rejected() {
    for offset in [1000, -1000] {
        let s = generate(SynthConfig {
            frame_count: 180,
            start_index: 10000,
            relocates: vec![(80, 10080 + offset)],
            corrupt_frames: vec![(50, 30000)],
            ..Default::default()
        });
        let mut d = Decoder::new(48000);
        let mut m = TcModel::new(48000, 25.);
        m.set_input_state(State::NoSignal);
        let mut count = 0;
        for (i, pcm) in s.pcm.chunks(128).enumerate() {
            let first = i * 128;
            m.add_stamp(IoStamp {
                first_sample: first as u64,
                host_ns: s.host_ns(first as u64),
                frames: pcm.len() as u32,
                discontinuity: false,
            });
            for e in d.feed_at(first as u64, pcm) {
                let now = s.host_ns(e.frame.sample_index);
                m.observe(e, now);
                if count == 50 {
                    assert_eq!(m.epoch, 0);
                }
                if count == 51 {
                    assert_eq!(m.rejected_frames, 1);
                }
                if count == 80 {
                    assert_eq!(m.epoch, 0);
                }
                if count == 81 {
                    assert_eq!(m.epoch, 1);
                    let snap = m.snapshot(now);
                    assert_eq!(snap.state, State::Locked);
                    assert!((snap.at(now).unwrap() - s.truth(now)).abs() < 0.02);
                }
                count += 1;
            }
        }
        assert_eq!(m.epoch, 1);
        assert_eq!(m.rejected_frames, 1);
    }
}
#[test]
fn state_transitions_and_eight_frame_freewheel() {
    let s = generate(SynthConfig {
        frame_count: 60,
        ..Default::default()
    });
    let mut m = run(&s, 32, false);
    let last = s.host_ns((s.pcm.len() - 192) as u64) + 100_000 + 1_000_000;
    assert_eq!(m.snapshot(last).state, State::Locked);
    assert_eq!(m.snapshot(last + 81_000_000).state, State::Freewheel);
    assert_eq!(m.snapshot(last + 300_000_000).state, State::Freewheel);
    assert_eq!(m.snapshot(last + 330_000_000).state, State::NoSignal);
    assert!(m.snapshot(last + 330_000_000).at(last).is_none());
    assert_eq!(m.dropouts, 1);
}
#[test]
fn host_mapping_from_io_stamps() {
    let mut h = HostMap::new(48000);
    for i in 0..100 {
        h.add_stamp(IoStamp {
            first_sample: i * 480,
            host_ns: 1_000_000_000 + i * 10_000_000,
            frames: 480,
            discontinuity: false,
        });
    }
    let (s, h0, k) = h.mapping().unwrap();
    assert!((h0 + k * (16000. - s) - 1_333_333_333.3333333).abs() < 0.01);
}
#[test]
fn hostmap_discontinuity_resets() {
    let mut h = HostMap::new(48000);
    for i in 0..4 {
        h.add_stamp(IoStamp {
            first_sample: i * 480,
            host_ns: i * 10_000_000,
            frames: 480,
            discontinuity: false,
        });
    }
    assert!(h.mapping().is_some());
    assert!(h.add_stamp(IoStamp {
        first_sample: 3000,
        host_ns: 100_000_000,
        frames: 480,
        discontinuity: false
    }));
    assert!(h.mapping().is_none());
}
#[test]
fn ring_gap_preserves_absolute_timeline() {
    let s = generate(SynthConfig::default());
    let mut d = Decoder::new(48000);
    d.feed_at(1_000_000, &s.pcm[..10000]);
    let events = d.feed_at(1_020_000, &s.pcm[20000..]);
    assert!(!events.is_empty());
    assert!(events[0].frame.sample_index > 1_020_000);
    assert!(events.last().unwrap().locked);
}
#[test]
fn monotonic_election_grace_and_duplicates() {
    let s = generate(SynthConfig::default());
    let mut session = Session::new(48000, 2, Channel::Auto);
    for (i, pcm) in s.pcm.chunks(2000).take(20).enumerate() {
        let stereo: Vec<i16> = pcm.iter().flat_map(|&s| [s, s]).collect();
        session.feed((i * 2000) as u64, &stereo, 0).unwrap();
    }
    assert!(session.duplicate_channels);
    assert_eq!(session.active_channel(), Some(1));
    session.tick(699_999_999);
    assert_eq!(session.active_channel(), Some(1));
    session.tick(700_000_000);
    assert_eq!(session.active_channel(), None);
}
#[test]
fn rate_resolution_ppm_and_project_tie_break() {
    assert_eq!(resolve_rate(23.976, 24, false, 24.).fps, 24000. / 1001.);
    assert_eq!(resolve_rate(24., 24, false, 23.976).fps, 24.);
    assert_eq!(resolve_rate(30., 30, true, 30.).fps, 30000. / 1001.);
    let r = resolve_rate(24.24, 24, false, 24000. / 1001.);
    assert!(r.ambiguous);
    assert_eq!(r.fps, 24000. / 1001.);
}
#[test]
fn completing_edge_and_input_latency_sign() {
    let s = generate(SynthConfig {
        frame_count: 130,
        rise_time_us: 0.,
        ..Default::default()
    });
    let mut m = run(&s, 32, false);
    let t = s.host_ns(s.pcm.len() as u64) + 100_000;
    let before = m.snapshot(t).at(t).unwrap();
    assert!((before - s.truth(t - 100_000)).abs() < 0.002);
    m.input_latency_ns = 20_000_000.;
    let after = m.snapshot(t).at(t).unwrap();
    assert!((after - before - 0.5).abs() < 1e-8);
}

#[test]
fn dropouts_reacquire_and_no_polling_does_not_extend_freewheel() {
    let s = generate(SynthConfig {
        frame_count: 180,
        dropouts: vec![50..54, 90..110],
        ..Default::default()
    });
    let mut d = Decoder::new(48000);
    let mut m = TcModel::new(48000, 25.);
    m.set_input_state(State::NoSignal);
    let mut saw_freewheel = false;
    let mut saw_loss = false;
    let mut saw_acquiring = false;
    for (i, pcm) in s.pcm.chunks(128).enumerate() {
        let first = i * 128;
        let now = s.host_ns((first + pcm.len()) as u64);
        m.add_stamp(IoStamp {
            first_sample: first as u64,
            host_ns: s.host_ns(first as u64),
            frames: pcm.len() as u32,
            discontinuity: false,
        });
        for e in d.feed_at(first as u64, pcm) {
            m.observe(e, now);
        }
        let snap = m.snapshot(now);
        saw_freewheel |= snap.state == State::Freewheel;
        saw_loss |= now > 4_000_000_000 && snap.state == State::NoSignal;
        saw_acquiring |= snap.state == State::Acquiring;
    }
    assert!(saw_freewheel && saw_loss && saw_acquiring);
    assert_eq!(
        m.snapshot(s.host_ns(s.pcm.len() as u64)).state,
        State::Locked
    );
}
#[test]
fn large_forward_relocate_does_not_become_previous_day() {
    let s = generate(SynthConfig {
        frame_count: 140,
        relocates: vec![(80, 20 * 3600 * 25)],
        ..Default::default()
    });
    let mut m = run(&s, 256, false);
    let now = s.host_ns(s.pcm.len() as u64) + 100_000;
    assert_eq!(m.epoch, 1);
    assert!(m.snapshot(now).at(now).unwrap() > 20. * 3600. * 25.);
}
#[test]
fn freewheel_zero_disables_extrapolation_after_detection() {
    let s = generate(SynthConfig {
        frame_count: 60,
        ..Default::default()
    });
    let mut m = run(&s, 32, false);
    m.freewheel_frames = 0;
    let now = s.host_ns(s.pcm.len() as u64) + 100_000 + 100_000_000;
    assert_eq!(m.snapshot(now).state, State::NoSignal);
}

#[test]
fn acquisition_silence_expires_without_a_fit() {
    use timecode_tools::{
        decoder::{DecodedFrame, Frame},
        label::Label,
    };
    let mut m = TcModel::new(48000, 25.);
    m.set_input_state(State::NoSignal);
    m.observe(
        DecodedFrame {
            frame: Frame {
                label: Label::from_index(0, 25, false),
                sample_index: 1920,
            },
            locked: false,
            estimated_fps: Some(25),
        },
        0,
    );
    assert_eq!(m.snapshot(0).state, State::Acquiring);
    assert_eq!(m.snapshot(81_000_000).state, State::NoSignal);
}
