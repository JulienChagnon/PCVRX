// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end tests of the receiver chain on synthetic signals.

use crate::testsig::*;
use crate::*;
use openvrx_core::{FieldParity, LanePreset, LineFlags, SampleFormat};

pub(crate) struct Run {
    pub lines: Vec<LineRecord>,
    pub gen: SigGen,
    pub dsp: RxDsp,
}

/// Pushes `samples` generated samples through a fresh chain.
pub(crate) fn run(spec: SigSpec, cfg: RxDspConfig, samples: usize, chunk: usize) -> Run {
    let gen = SigGen::new(spec);
    let dsp = RxDsp::new(0, cfg);
    let mut r = Run {
        lines: Vec::new(),
        gen,
        dsp,
    };
    feed(&mut r, samples, chunk);
    r
}

pub(crate) fn feed(r: &mut Run, samples: usize, chunk: usize) {
    let mut done = 0;
    while done < samples {
        let n = chunk.min(samples - done);
        let c = r.gen.next(n);
        r.dsp.push_bytes(&c.bytes, 0, &mut r.lines);
        done += n;
    }
}

pub(crate) fn cfg_for(spec: &SigSpec) -> RxDspConfig {
    RxDspConfig {
        sample_format: spec.sample_format.clone(),
        ..RxDspConfig::default()
    }
}

fn secs(spec: &SigSpec, s: f64) -> usize {
    (spec.sample_format.sample_rate_hz() * s) as usize
}

/// The generated line nearest to a record's 0H.
fn nearest(r: &Run, l: &LineRecord) -> LineTruth {
    *r.gen
        .lines
        .iter()
        .min_by(|a, b| (a.h0 - l.src_position).abs().total_cmp(&(b.h0 - l.src_position).abs()))
        .unwrap()
}

/// Timing error statistics (input samples): mean and RMS about the mean,
/// over lines with a measured pulse.
fn timing(r: &Run, from: usize) -> (f64, f64, usize) {
    let errs: Vec<f64> = r.lines[from..]
        .iter()
        .filter(|l| !l.quality.flags.contains(LineFlags::FLYWHEEL))
        .map(|l| l.src_position - nearest(r, l).h0)
        .collect();
    let n = errs.len().max(1) as f64;
    let mean = errs.iter().sum::<f64>() / n;
    let rms = (errs.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / n).sqrt();
    (mean, rms, errs.len())
}

/// Picture error against the clean video over the active part of picture
/// lines: (truth variance, mean squared error), IRE^2. `shift` moves the
/// truth sampling by a fraction of a grid sample (fixed channel delay).
fn picture_error(r: &Run, from: usize, shift: f64) -> (f64, f64, usize) {
    let (mut sv, mut se, mut n) = (0.0f64, 0.0f64, 0usize);
    let mut vals = Vec::new();
    for l in &r.lines[from..] {
        if l.quality.flags.contains(LineFlags::VBI) {
            continue;
        }
        let p = l.standard.params();
        let spl = p.samples_per_line;
        let step = l.src_line_len / spl as f64;
        let (truth, fl) = r.gen.truth_line(l.src_position - shift * step, spl);
        if fl != l.frame_line {
            continue;
        }
        // Common band limit (luma + chroma band) for both.
        let h = crate::filters::design_lowpass(41, 4.2e6 / p.grid_rate_hz(), 6.0);
        let lp = |x: &[f32]| -> Vec<f64> {
            (0..x.len())
                .map(|k| {
                    h.iter()
                        .enumerate()
                        .map(|(j, &c)| c as f64 * x[(k + j).saturating_sub(20).min(x.len() - 1)] as f64)
                        .sum()
                })
                .collect()
        };
        let (ys, ts) = (lp(&l.samples), lp(&truth));
        let a = p.active_range();
        for k in a.start + 24..a.end - 24 {
            let e = ys[k] - ts[k];
            let truth = &ts;
            se += e * e;
            vals.push(truth[k]);
            n += 1;
        }
    }
    if n > 0 {
        let m = vals.iter().sum::<f64>() / n as f64;
        sv = vals.iter().map(|v| (v - m).powi(2)).sum::<f64>() / n as f64;
    }
    (sv, se / n.max(1) as f64, n)
}

/// Share (per mille) of active picture samples more than 40 IRE away from
/// the clean video: the visible sparkles of FM clicks.
fn sparkles(r: &Run, from: usize, shift: f64) -> f64 {
    let (mut big, mut n) = (0usize, 0usize);
    for l in &r.lines[from..] {
        if l.quality.flags.contains(LineFlags::VBI) {
            continue;
        }
        let p = l.standard.params();
        let spl = p.samples_per_line;
        let (truth, fl) = r.gen.truth_line(l.src_position - shift * l.src_line_len / spl as f64, spl);
        if fl != l.frame_line {
            continue;
        }
        for k in p.active_range() {
            big += ((l.samples[k] - truth[k]).abs() > 40.0) as usize;
            n += 1;
        }
    }
    1000.0 * big as f64 / n.max(1) as f64
}

/// Fractional truth shift (grid samples) that best matches a clean run.
fn best_shift(r: &Run, from: usize) -> f64 {
    let mut best = (f64::INFINITY, 0.0);
    for k in -30..=10 {
        let s = k as f64 * 0.1;
        let (_, mse, _) = picture_error(r, from, s);
        if mse < best.0 {
            best = (mse, s);
        }
    }
    best.1
}

fn sinad_db(r: &Run, from: usize, shift: f64) -> f64 {
    let (v, mse, _) = picture_error(r, from, shift);
    10.0 * (v / mse.max(1e-12)).log10()
}

fn check_lock(std: VideoStandard, auto: bool) {
    let spec = SigSpec {
        standard: std,
        cn_db: Some(30.0),
        start_line: 100,
        ..SigSpec::default()
    };
    let mut cfg = cfg_for(&spec);
    if !auto {
        cfg.standard = Some(std);
    }
    let n = secs(&spec, 0.12);
    let r = run(spec, cfg, n, 8192);
    let st = r.dsp.stats();
    assert_eq!(st.standard, Some(std));
    assert_eq!(st.lock, LockState::Locked);
    let p = std.params();
    assert!((st.line_rate_hz / p.line_hz - 1.0).abs() < 1e-5, "line rate {}", st.line_rate_hz);
    assert!((st.field_rate_hz / p.field_hz - 1.0).abs() < 0.01, "field rate {}", st.field_rate_hz);
    assert!(r.lines.len() as f64 > 0.06 * p.line_hz, "{} lines", r.lines.len());
    // Every line carries the right frame line, parity and field line.
    let mut seen_first = false;
    let mut seen_second = false;
    for l in &r.lines {
        let t = nearest(&r, l);
        assert!((l.src_position - t.h0).abs() < 2.0, "0H off by {}", l.src_position - t.h0);
        assert_eq!(l.frame_line, t.frame_line);
        assert_eq!(l.parity, t.parity);
        assert_eq!(p.frame_line(l.parity, l.field_line as usize), l.frame_line);
        assert_eq!(l.samples.len(), p.samples_per_line);
        assert_eq!(l.damage.len(), p.samples_per_line);
        seen_first |= l.parity == FieldParity::First;
        seen_second |= l.parity == FieldParity::Second;
        let vbi = l.quality.flags.contains(LineFlags::VBI);
        let row = p.output_row(l.frame_line);
        assert_eq!(vbi, row.is_none(), "VBI flag on frame line {}", l.frame_line);
    }
    assert!(seen_first && seen_second);
    // Field counter increments once per field.
    let fs: Vec<u64> = r.lines.iter().map(|l| l.field_seq).collect();
    for w in r.lines.windows(2) {
        let new_field = w[1].field_line == 0;
        assert_eq!(w[1].field_seq, w[0].field_seq + new_field as u64);
    }
    assert!(fs.last().unwrap() - fs[0] >= 5);
    // Timing (after the flywheel settled) and levels.
    let (_, rms, cnt) = timing(&r, 200);
    let grid_step = r.dsp.config().sample_rate_hz() / p.grid_rate_hz();
    assert!(cnt > 500);
    assert!(rms / grid_step < 0.05, "{std:?} timing RMS {} grid samples", rms / grid_step);
    let tail = &r.lines[r.lines.len() / 2..];
    let porch: Vec<f32> = tail
        .iter()
        .filter(|l| !l.quality.flags.contains(LineFlags::VBI))
        .map(|l| {
            let b = p.back_porch_range();
            l.samples[b.clone()].iter().sum::<f32>() / b.len() as f32
        })
        .collect();
    let mean_porch = porch.iter().sum::<f32>() / porch.len() as f32;
    assert!(mean_porch.abs() < 0.5, "porch {mean_porch}");
    let tip: f32 = tail
        .iter()
        .filter(|l| !l.quality.flags.contains(LineFlags::VBI))
        .map(|l| {
            let t = p.sync_tip_range();
            l.samples[t.clone()].iter().sum::<f32>() / t.len() as f32
        })
        .sum::<f32>()
        / porch.len() as f32;
    assert!((tip - p.sync_ire).abs() < 0.5, "tip {tip}");
    let shift = best_shift(&r, 200);
    let s = sinad_db(&r, 200, shift);
    assert!(s > 24.0, "{std:?} SINAD at 30 dB C/N: {s}");
    let q = &tail
        .iter()
        .find(|l| !l.quality.flags.contains(LineFlags::VBI))
        .unwrap()
        .quality;
    assert!(q.noise_ire < 1.5, "noise {}", q.noise_ire);
    assert!(q.cn_db > 18.0, "cn {}", q.cn_db);
    assert!(q.sync_score > 0.7, "sync score {}", q.sync_score);
    assert!(q.weight > 0.3);
}

#[test]
fn ntsc_locks_numbers_lines_and_times_edges() {
    check_lock(VideoStandard::Ntsc, true);
}

#[test]
fn pal_locks_numbers_lines_and_times_edges() {
    check_lock(VideoStandard::Pal, true);
}

#[test]
fn standard_override_is_respected() {
    check_lock(VideoStandard::Pal, false);
    // NTSC signal with a PAL override never locks.
    let spec = SigSpec {
        cn_db: Some(30.0),
        ..SigSpec::default()
    };
    let mut cfg = cfg_for(&spec);
    cfg.standard = Some(VideoStandard::Pal);
    let n = secs(&spec, 0.06);
    let r = run(spec, cfg, n, 8192);
    assert!(r.lines.is_empty());
    assert_eq!(r.dsp.stats().lock, LockState::Searching);
}

#[test]
fn deviation_and_offset_variants_normalise_levels() {
    for (dev, blank, ppm) in [(0.7, -1.5e6, 40.0), (1.4, -0.3e6, -40.0)] {
        let spec = SigSpec {
            deviation: dev,
            blank_hz: blank,
            tx_ppm: ppm,
            rx_ppm: -ppm / 2.0,
            cn_db: Some(25.0),
            picture: Picture::Bars,
            ..SigSpec::default()
        };
        let cfg = cfg_for(&spec);
        let n = secs(&spec, 0.1);
        let r = run(spec, cfg, n, 16384);
        let st = r.dsp.stats();
        assert_eq!(st.lock, LockState::Locked, "dev {dev}");
        assert!((st.deviation_scale - dev as f32).abs() < 0.05, "deviation {}", st.deviation_scale);
        assert!((st.carrier_offset_hz - blank as f32).abs() < 30e3, "carrier {}", st.carrier_offset_hz);
        // AFC centred the signal: sync tip and white equally far from 0 Hz.
        let sync = 40.0 * dev * HZ_PER_IRE;
        let centre = blank - sync + 0.5 * 140.0 * dev * HZ_PER_IRE;
        assert!((st.nco_shift_hz as f64 - centre).abs() < 60e3, "nco {} vs {centre}", st.nco_shift_hz);
        let shift = best_shift(&r, 300);
        let s = sinad_db(&r, 300, shift);
        assert!(s > 17.0, "dev {dev} SINAD {s}");
        for l in &r.lines[300..] {
            assert_eq!(l.frame_line, nearest(&r, l).frame_line);
        }
    }
}

#[test]
fn coasts_through_carrier_dropout() {
    let spec = SigSpec {
        cn_db: Some(25.0),
        ..SigSpec::default()
    };
    let cfg = cfg_for(&spec);
    let line = (spec.sample_format.sample_rate_hz() / VideoStandard::Ntsc.params().line_hz) as usize;
    let mut r = run(spec.clone(), cfg, secs(&spec, 0.06), 4096);
    let before = r.lines.len();
    // 30 lines of no carrier (noise 20 dB above the old carrier, ADC level kept).
    r.gen.set_cn(Some(-20.0));
    r.gen.set_gain_dc(18.0, (0.0, 0.0));
    feed(&mut r, 30 * line, 4096);
    r.gen.set_cn(Some(25.0));
    r.gen.set_gain_dc(180.0, (0.0, 0.0));
    feed(&mut r, secs(&spec, 0.04), 4096);
    let lost: Vec<&LineRecord> = r.lines[before..]
        .iter()
        .filter(|l| l.quality.flags.contains(LineFlags::NO_CARRIER))
        .collect();
    assert!(lost.len() >= 20, "no-carrier lines {}", lost.len());
    for l in &lost {
        assert!(l.damage.iter().all(|&d| d >= damage::SEVERE));
        assert_eq!(l.quality.weight, 0.0);
    }
    // Lines keep coming at the right positions with the right numbers
    // during and after the dropout.
    for l in &r.lines[before..] {
        let t = nearest(&r, l);
        assert!((l.src_position - t.h0).abs() < 3.0, "0H drift {}", l.src_position - t.h0);
        assert_eq!(l.frame_line, t.frame_line);
    }
    let tail = &r.lines[r.lines.len() - 100..];
    assert!(tail.iter().all(|l| !l.quality.flags.contains(LineFlags::FLYWHEEL)
        || l.quality.flags.contains(LineFlags::VBI)));
    assert_eq!(r.dsp.stats().lock, LockState::Locked);
}

/// The demodulated levels move while the carrier stays strong (sync depth
/// less than half, everything shifted up) so that the slicer threshold of
/// the old levels lies below every sample: no pulse is seen at all. The
/// tracker must search again within a few lines instead of coasting blind
/// for `coast_lines`.
#[test]
fn relevels_when_the_slicer_sees_no_pulses_at_good_cn() {
    let spec = SigSpec {
        cn_db: Some(28.0),
        ..SigSpec::default()
    };
    let line = (spec.sample_format.sample_rate_hz() / VideoStandard::Ntsc.params().line_hz) as usize;
    let outcome = |blind_lines: u32| {
        let mut cfg = cfg_for(&spec);
        cfg.sync.blind_lines = blind_lines;
        let mut r = run(spec.clone(), cfg, secs(&spec, 0.08), 4096);
        assert_eq!(r.dsp.stats().lock, LockState::Locked);
        let before = r.lines.len();
        // Old: tip -2.9 MHz, blank -1.0 MHz, slicer about -2.0 MHz. New:
        // tip -1.4 MHz, so nothing crosses the old slicer.
        r.gen.set_modulation(0.45, -0.5e6);
        feed(&mut r, 120 * line, 4096);
        let after: Vec<&LineRecord> = r.lines[before..].iter().collect();
        // The first line from which every non-VBI line has a measured pulse again.
        let last_fly = after
            .iter()
            .rposition(|l| l.quality.flags.contains(LineFlags::FLYWHEEL) && !l.quality.flags.contains(LineFlags::VBI))
            .map_or(0, |i| i + 1);
        (last_fly, after.len())
    };
    let (with, n) = outcome(SyncConfig::default().blind_lines);
    assert!(n > 100, "{n} lines");
    assert!(with < 40, "still coasting {with} lines after the level change");
    // Without the check the flywheel coasts blind past the end of the run.
    let (without, n) = outcome(0);
    assert!(without + 5 >= n, "without the check it recovered after {without} of {n} lines");
}

/// Line-start error against the camera's true line start, over every
/// emitted line after `from` (coasted lines included: they are shown too):
/// RMS about the mean in us, and the share of FLYWHEEL lines.
fn start_error(r: &Run, from: usize) -> (f64, f64) {
    let us = 1e6 / r.gen.sample_rate();
    let ls = &r.lines[from..];
    let errs: Vec<f64> = ls.iter().map(|l| (l.src_position - nearest(r, l).h0) * us).collect();
    let n = errs.len().max(1) as f64;
    let mean = errs.iter().sum::<f64>() / n;
    let rms = (errs.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / n).sqrt();
    let fly = ls.iter().filter(|l| l.quality.flags.contains(LineFlags::FLYWHEEL)).count() as f64 / n;
    (rms, fly)
}

fn wobble_run(gate_us: f32, cn: f64, wobble_us: f64) -> (f64, f64) {
    wobble_run_pn(gate_us, SyncConfig::default().phase_noise, cn, wobble_us)
}

fn wobble_run_pn(gate_us: f32, phase_noise: f32, cn: f64, wobble_us: f64) -> (f64, f64) {
    let spec = SigSpec {
        cn_db: Some(cn),
        ..SigSpec::default()
    };
    let mut cfg = cfg_for(&spec);
    cfg.sync.gate_min_us = gate_us;
    cfg.sync.phase_noise = phase_noise;
    // The gate alone: cutting lines from their own edges (follow_edge_us,
    // tested by cuts_wobbling_lines_from_their_own_edge) hides the start
    // error difference between gates.
    cfg.sync.follow_edge_us = 0.0;
    let mut r = run(spec.clone(), cfg, secs(&spec, 0.04), 4096);
    let from = r.lines.len();
    r.gen.set_line_wobble(wobble_us, 7);
    feed(&mut r, secs(&spec, 0.1), 4096);
    start_error(&r, from + 30)
}

/// A camera whose line timing wobbles by a few tenths of a microsecond:
/// the gate takes its pulses as measurements instead of coasting on the
/// prediction and snapping back with relocks.
#[test]
fn accepts_camera_line_timing_wobble() {
    let (rms_new, fly_new) = wobble_run(SyncConfig::default().gate_min_us, 25.0, 0.3);
    let (rms_old, fly_old) = wobble_run(0.08, 25.0, 0.3);
    assert!(fly_new < 0.2 * fly_old, "flywheel {fly_new:.3} vs {fly_old:.3} with the narrow gate");
    assert!(rms_new < 0.9 * rms_old, "start error {rms_new:.3} us vs {rms_old:.3} us");
}

fn follow_run(follow_us: f32, cn: f64, wobble_us: f64) -> (f64, f64) {
    follow_run_sigma(follow_us, SyncConfig::default().follow_sigma_us, cn, wobble_us)
}

fn follow_run_sigma(follow_us: f32, sigma_us: f32, cn: f64, wobble_us: f64) -> (f64, f64) {
    let spec = SigSpec {
        cn_db: Some(cn),
        ..SigSpec::default()
    };
    let mut cfg = cfg_for(&spec);
    cfg.sync.follow_edge_us = follow_us;
    cfg.sync.follow_sigma_us = sigma_us;
    let mut r = run(spec.clone(), cfg, secs(&spec, 0.04), 4096);
    let from = r.lines.len();
    if wobble_us > 0.0 {
        r.gen.set_line_wobble(wobble_us, 7);
    }
    feed(&mut r, secs(&spec, 0.1), 4096);
    start_error(&r, from + 30)
}

/// A camera or converter whose single lines start late or early.
/// The flywheel stays smooth, but each line is cut from its own precise
/// edge, so its picture is not shifted sideways by the wobble.
#[test]
fn cuts_wobbling_lines_from_their_own_edge() {
    let (on, _) = follow_run(SyncConfig::default().follow_edge_us, 25.0, 0.3);
    let (off, _) = follow_run(0.0, 25.0, 0.3);
    assert!(on < 0.3 * off, "start error {on:.3} us with follow vs {off:.3} us without");
}

/// On a noisy signal without wobble one edge is less precise than the
/// flywheel: following may add only a little jitter (0.015 us is about 0.2
/// px; the wobble it removes is ten times that, see diag_follow), and
/// nearly none on a clean signal (the dead zone).
#[test]
fn follow_edge_adds_little_jitter_without_wobble() {
    for (cn, extra) in [(10.0, 0.015), (18.0, 0.005), (30.0, 0.001)] {
        let (on, _) = follow_run(SyncConfig::default().follow_edge_us, cn, 0.0);
        let (off, _) = follow_run(0.0, cn, 0.0);
        assert!(on <= off + extra, "C/N {cn}: start error {on:.4} us with follow vs {off:.4} us without");
    }
}

/// Follow sweep: `cargo test -p openvrx-dsp --release -- --ignored diag_follow --nocapture`;
/// FS=0.03,0.015 sets the follow_sigma_us values (default: the config's).
#[test]
#[ignore]
fn diag_follow() {
    let sigmas: Vec<f32> = std::env::var("FS")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![SyncConfig::default().follow_sigma_us]);
    for &fs in &sigmas {
        for cn in [10.0, 14.0, 18.0, 25.0, 35.0] {
            for wob in [0.0, 0.15, 0.3, 0.5] {
                let (on, fly) = follow_run_sigma(SyncConfig::default().follow_edge_us, fs, cn, wob);
                let (off, _) = follow_run(0.0, cn, wob);
                println!(
                    "sigma {fs:.3} cn {cn:4.1} wobble {wob:.2} us: start error rms {off:.4} -> {on:.4} us (flywheel {fly:.3})"
                );
            }
        }
    }
}

/// Gate floor against wobble and C/N: `cargo test -p openvrx-dsp --release -- --ignored diag_gate --nocapture`.
#[test]
#[ignore]
fn diag_gate_wobble() {
    // PN: flywheel phase noise values (input samples). A large one makes
    // the line start follow a camera's wobble on a clean signal, but it also
    // follows edge noise (SINAD), blurs the period (line rate) and lets the
    // prediction wander onto noise pulses in a dropout, so it stays small.
    let pns: Vec<f32> = std::env::var("PN")
        .ok()
        .map_or(vec![SyncConfig::default().phase_noise], |s| s.split(',').filter_map(|x| x.parse().ok()).collect());
    let gates: Vec<f32> = std::env::var("GATES").ok().map_or(vec![0.08, 0.25, 0.4, 0.6], |s| s.split(',').filter_map(|x| x.parse().ok()).collect());
    for pn in pns {
        for wob in [0.0, 0.3, 0.5] {
            for cn in [25.0, 14.0, 10.0, 8.0] {
                let mut row = format!("pn {pn} wobble {wob:.1} us, C/N {cn:4.1}:");
                for &gate in &gates {
                    let (rms, fly) = wobble_run_pn(gate, pn, cn, wob);
                    row += &format!("  gate {gate:.2}: rms {rms:.3} us fly {:4.1} %", 100.0 * fly);
                }
                println!("{row}");
            }
        }
    }
}

#[test]
fn recovers_from_usb_gaps() {
    let spec = SigSpec {
        cn_db: Some(30.0),
        ..SigSpec::default()
    };
    let cfg = cfg_for(&spec);
    let mut r = run(spec.clone(), cfg, secs(&spec, 0.06), 8192);
    // Short gap: 3000 samples lost (about 3.5 lines).
    let _ = r.gen.next(3000);
    r.dsp.mark_gap(3000);
    let before = r.lines.len();
    feed(&mut r, secs(&spec, 0.03), 8192);
    let flagged = r.lines[before..]
        .iter()
        .filter(|l| l.quality.flags.contains(LineFlags::SAMPLES_LOST))
        .count();
    assert!((3..=8).contains(&flagged), "lost lines {flagged}");
    for l in &r.lines[before..] {
        let t = nearest(&r, l);
        assert!((l.src_position - t.h0).abs() < 2.0);
        assert_eq!(l.frame_line, t.frame_line);
    }
    // Medium gap: one field lost (coasted through in pieces): the lines
    // inside it come out flagged, and the numbering continues.
    let lost = secs(&spec, 0.0167);
    let _ = r.gen.next(lost);
    r.dsp.mark_gap(lost as u64);
    let before = r.lines.len();
    feed(&mut r, secs(&spec, 0.03), 8192);
    let flagged = r.lines[before..]
        .iter()
        .filter(|l| l.quality.flags.contains(LineFlags::SAMPLES_LOST))
        .count();
    assert!((255..=270).contains(&flagged), "lost lines {flagged}");
    for l in r.lines[before..].iter().filter(|l| !l.quality.flags.contains(LineFlags::SAMPLES_LOST)) {
        let t = nearest(&r, l);
        assert!((l.src_position - t.h0).abs() < 2.0, "after medium gap {}", l.src_position - t.h0);
        assert_eq!(l.frame_line, t.frame_line);
    }
    // Long gap: 0.2 s lost, full re-acquisition with correct numbering.
    let lost = secs(&spec, 0.2);
    let _ = r.gen.next(lost);
    r.dsp.mark_gap(lost as u64);
    let before = r.lines.len();
    feed(&mut r, secs(&spec, 0.08), 8192);
    assert!(r.lines.len() - before > 500);
    for l in &r.lines[before..] {
        let t = nearest(&r, l);
        assert!((l.src_position - t.h0).abs() < 2.0, "after long gap {}", l.src_position - t.h0);
        assert_eq!(l.frame_line, t.frame_line);
    }
    let st = r.dsp.stats();
    assert_eq!(st.gaps, 3);
    assert_eq!(st.lost_samples, 3000 + secs(&spec, 0.0167) as u64 + lost as u64);
}

#[test]
fn gain_step_dc_recovery() {
    let spec = SigSpec {
        cn_db: Some(25.0),
        dc: (20.0, -3.0),
        sample_format: SampleFormat::from_preset(LanePreset::I8Q8, 18),
        ..SigSpec::default()
    };
    let mut cfg = cfg_for(&spec);
    cfg.stats_window_ms = 5.0;
    let mut r = run(spec.clone(), cfg, secs(&spec, 0.05), 8192);
    let (di, dq) = (r.dsp.stats().i_dc, r.dsp.stats().q_dc);
    assert!((di - 20.0).abs() < 1.5 && (dq + 3.0).abs() < 1.5, "dc {di} {dq}");
    r.dsp.note_event(RxEvent::GainChanged { index: 68 });
    feed(&mut r, 4000, 4000);
    r.gen.set_gain_dc(250.0, (32.0, -1.0));
    let before = r.lines.len();
    feed(&mut r, secs(&spec, 0.03), 8192);
    let st = r.dsp.stats();
    assert_eq!(st.gain_steps, 1);
    let flagged: Vec<&LineRecord> = r.lines[before.saturating_sub(5)..]
        .iter()
        .filter(|l| l.quality.flags.contains(LineFlags::GAIN_STEP))
        .collect();
    assert_eq!(flagged.len(), 1, "GAIN_STEP lines");
    assert!((st.i_dc - 32.0).abs() < 1.5 && (st.q_dc + 1.0).abs() < 1.5, "dc {} {}", st.i_dc, st.q_dc);
    // The picture after the step is as good as before.
    let shift = best_shift(&r, 200);
    let after = sinad_db(&r, before + 5, shift);
    assert!(after > 22.0, "SINAD after the step {after}");
}

#[test]
fn every_lane_preset_locks() {
    for p in LanePreset::ALL {
        let divider = if p.format().word_bytes == 1 { 9 } else { 18 };
        let rms = match p {
            LanePreset::I4Q4Ultra => 60.0,
            LanePreset::I4Q4Fine => 120.0,
            LanePreset::Angle8 => 60.0,
            _ => 220.0,
        };
        let spec = SigSpec {
            cn_db: Some(25.0),
            rms_adc: rms,
            sample_format: SampleFormat::from_preset(p, divider),
            ..SigSpec::default()
        };
        let mut cfg = cfg_for(&spec);
        if p == LanePreset::Angle8 {
            cfg.demod = DemodKind::LutAngle;
        }
        let n = secs(&spec, 0.07);
        let r = run(spec, cfg, n, 8192);
        assert_eq!(r.dsp.stats().lock, LockState::Locked, "{p}");
        let shift = best_shift(&r, 200);
        let s = sinad_db(&r, 200, shift);
        assert!(s > 16.0, "{p}: SINAD {s}");
        if p == LanePreset::Angle8 {
            assert!(r.lines[300].quality.cn_db.is_nan());
            assert_eq!(r.dsp.stats().demod, DemodKind::LutAngle);
        }
    }
}

#[test]
fn every_demodulator_locks_at_good_cn() {
    for d in DemodKind::ALL {
        let spec = SigSpec {
            cn_db: Some(20.0),
            ..SigSpec::default()
        };
        let mut cfg = cfg_for(&spec);
        cfg.demod = d;
        let n = secs(&spec, 0.07);
        let r = run(spec, cfg, n, 8192);
        assert_eq!(r.dsp.stats().lock, LockState::Locked, "{d:?}");
        let shift = best_shift(&r, 200);
        let s = sinad_db(&r, 200, shift);
        // The LS estimator's 3-difference window is a low-pass at 13.33 MS/s
        // (detail and chroma are attenuated), so its bar is lower.
        let bar = if d == DemodKind::LeastSquares { 10.0 } else { 17.0 };
        assert!(s > bar, "{d:?}: SINAD {s}");
    }
}

#[test]
fn click_repair_counts_clicks_and_reduces_sparkles() {
    let mut res = Vec::new();
    for enabled in [false, true] {
        let spec = SigSpec {
            cn_db: Some(6.0),
            seed: 7,
            ..SigSpec::default()
        };
        let mut cfg = cfg_for(&spec);
        cfg.clicks.enabled = enabled;
        let n = secs(&spec, 0.08);
        let r = run(spec, cfg, n, 8192);
        let clicks: u32 = r.lines.iter().map(|l| l.quality.clicks as u32).sum();
        let repaired = r
            .lines
            .iter()
            .flat_map(|l| l.damage.iter())
            .filter(|&&d| d == damage::REPAIRED)
            .count();
        let (_, mse, _) = picture_error(&r, 200, -0.1);
        res.push((mse, sparkles(&r, 200, -0.1), clicks, repaired, r.dsp.stats().lock));
    }
    let (off, on) = (res[0], res[1]);
    assert_eq!(on.4, LockState::Locked);
    assert!(on.2 > 100 && on.3 > 100, "clicks {} repaired samples {}", on.2, on.3);
    assert_eq!(off.2, 0);
    assert!(on.1 < off.1, "sparkles {} vs {}", on.1, off.1);
    assert!(on.0 < off.0, "MSE {} vs {}", on.0, off.0);
}

#[test]
fn phase_probe_separates_clean_and_mixed_reads() {
    for preset in [LanePreset::I7Q7, LanePreset::I4Q4Top] {
        let fmt = SampleFormat::from_preset(preset, 6);
        let mixed_mask = match preset {
            LanePreset::I4Q4Top => 0xff,
            _ => 0xfefe,
        };
        for (cn, rms) in [(Some(25.0), 200.0), (None, 120.0)] {
            let base = SigSpec {
                cn_db: cn,
                rms_adc: rms,
                sample_format: fmt.clone(),
                channel_hz: Some(8e6),
                ..SigSpec::default()
            };
            let clean = SigGen::new(base.clone()).next(4096).bytes;
            let (v, s) = probe::verdict(&clean, &fmt.lanes);
            assert_eq!(v, probe::Verdict::Clean, "{preset} clean score {s}");
            let mixed = SigGen::new(SigSpec {
                mixed_mask,
                mixed_random: 0.5,
                ..base
            })
            .next(4096)
            .bytes;
            let (_, sm) = probe::verdict(&mixed, &fmt.lanes);
            // The simulator's mixed reads (random old/new bits from adjacent
            // 80 MS/s bus samples) are milder than the board's 19-80, so the
            // check is on separation, not on the board threshold.
            assert!(sm > 6.0 && sm > 20.0 * s.max(0.05), "{preset} mixed score {sm} vs clean {s}");
        }
    }
}

#[test]
fn spectrum_and_demod_taps() {
    let spec = SigSpec {
        cn_db: Some(25.0),
        ..SigSpec::default()
    };
    let mut cfg = cfg_for(&spec);
    cfg.spectrum.enabled = true;
    cfg.spectrum.rate_hz = 2000.0;
    let mut r = run(spec.clone(), cfg, 20000, 8192);
    r.dsp.set_demod_tap(Some(TapPoint::Video));
    feed(&mut r, 50000, 8192);
    let sp = r.dsp.take_spectrum().expect("spectrum");
    assert_eq!(sp.len(), 1024);
    let tap = r.dsp.demod_tap().expect("tap");
    assert_eq!(tap.freq_hz.len(), tap.mag.len());
    assert_eq!(tap.freq_hz.len(), tap.damage.len());
    assert!(tap.freq_hz.len() >= 49000);
    // Absolute frequencies: blanking near -1 MHz, sync near -2.9 MHz.
    let mut f = tap.freq_hz.clone();
    f.sort_by(|a, b| a.total_cmp(b));
    let tip = f[f.len() / 50];
    assert!((tip + 2.91e6).abs() < 0.1e6, "sync tip {tip}");
}

/// Luma SINAD against C/N per demodulator (the table in `demod.rs`).
/// Run with `cargo test -p openvrx-dsp --release sinad_table -- --ignored --nocapture`.
#[test]
#[ignore]
fn sinad_table() {
    let cns = [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 15.0, 20.0, 30.0];
    for (preset, divider) in [(LanePreset::I8Q8, 18u32), (LanePreset::I4Q4Top, 6)] {
        println!("{preset} at {:.2} MS/s", 240.0 / divider as f64);
        print!("{:>6}", "C/N");
        for d in DemodKind::ALL {
            print!("{:>8}", d.name());
        }
        println!();
        let mut shifts = std::collections::HashMap::new();
        for &cn in &cns {
            print!("{cn:>6.0}");
            for d in DemodKind::ALL {
                let spec = SigSpec {
                    cn_db: Some(cn),
                    seed: 11,
                    sample_format: SampleFormat::from_preset(preset, divider),
                    rms_adc: if preset == LanePreset::I4Q4Top { 300.0 } else { 220.0 },
                    ..SigSpec::default()
                };
                let mut cfg = cfg_for(&spec);
                cfg.demod = d;
                let n = secs(&spec, 0.08);
                let r = run(spec, cfg, n, 16384);
                let shift = *shifts.entry(d.name()).or_insert_with(|| {
                    let clean = SigSpec {
                        cn_db: None,
                        sample_format: SampleFormat::from_preset(preset, divider),
                        rms_adc: if preset == LanePreset::I4Q4Top { 300.0 } else { 220.0 },
                        ..SigSpec::default()
                    };
                    let mut c = cfg_for(&clean);
                    c.demod = d;
                    best_shift(&run(clean, c, secs(&SigSpec::default(), 0.05), 16384), 200)
                });
                let s = if r.lines.len() > 300 { sinad_db(&r, 200, shift) } else { f64::NAN };
                print!("{s:>8.1}");
            }
            println!();
        }
    }
}




#[test]
#[ignore]
fn diag_tracking() {
    for (preset, div) in [(LanePreset::I8Q8, 18u32), (LanePreset::I4Q4Top, 6)] {
        for d in [DemodKind::Polar, DemodKind::Pll, DemodKind::Ekf] {
            for bw in [2.0e6, 3.0e6, 4.0e6, 6.0e6, 9.0e6] {
                if d == DemodKind::Polar && bw > 2.0e6 {
                    continue;
                }
                let mut row = format!("{preset} {d:?} bw {:.0} MHz:", bw / 1e6);
                for cn in [6.0, 10.0, 20.0] {
                    let spec = SigSpec {
                        cn_db: Some(cn),
                        sample_format: SampleFormat::from_preset(preset, div),
                        rms_adc: if div == 6 { 300.0 } else { 220.0 },
                        ..SigSpec::default()
                    };
                    let mut cfg = cfg_for(&spec);
                    cfg.demod = d;
                    cfg.pll.natural_hz = bw as f32;
                    cfg.ekf.bandwidth_hz = bw as f32;
                    let n = secs(&spec, 0.05);
                    let r = run(spec, cfg, n, 16384);
                    let s = if r.lines.len() > 300 {
                        let sh = best_shift(&r, 200);
                        sinad_db(&r, 200, sh)
                    } else {
                        f64::NAN
                    };
                    row += &format!(" {cn}dB {s:.1}");
                }
                println!("{row}");
            }
        }
    }
}

#[test]
#[ignore]
fn diag_demod_isolated() {
    use crate::demod::{ChannelState, Demod};
    for cn in [10.0, 20.0, 40.0] {
        let spec = SigSpec { cn_db: Some(cn), ..SigSpec::default() };
        let mut g = SigGen::new(spec.clone());
        let c = g.next(200_000);
        let fs = spec.sample_format.sample_rate_hz();
        let mut row = format!("cn {cn}:");
        for (name, kind, nat) in [
            ("polar", DemodKind::Polar, 0.0),
            ("pll2.4", DemodKind::Pll, 2.4e6),
            ("pll6", DemodKind::Pll, 6e6),
            ("ekf3", DemodKind::Ekf, 3e6),
            ("ekf9", DemodKind::Ekf, 9e6),
        ] {
            let pll = PllConfig { natural_hz: nat as f32, ..PllConfig::default() };
            let ekf = EkfConfig { bandwidth_hz: nat as f32, ..EkfConfig::default() };
            let mut d = Demod::new(kind, fs, None, &LsConfig::default(), &pll, &ekf);
            let p: f64 = c.re.iter().zip(&c.im).map(|(a, b)| (a * a + b * b) as f64).sum::<f64>() / c.re.len() as f64;
            let cl = 10f64.powf(cn / 10.0);
            let mut out = vec![0.0; c.re.len()];
            d.process(&c.re, &c.im, ChannelState { power: p, cn: cl }, &mut out);
            let h = crate::filters::design_lowpass(31, 4.0e6 / fs, 6.0);
            let mut f = crate::filters::SymFir::new(h.clone());
            let mut y = vec![0.0; out.len()];
            f.process(&out, &mut y);
            let mut ft = crate::filters::SymFir::new(h);
            let mut t = vec![0.0; out.len()];
            ft.process(&c.truth, &mut t);
            let mut best = f64::NEG_INFINITY;
            for lag in 0..6usize {
                // Least-squares gain/offset fit.
                let n = y.len() - 1000;
                let (mut sx, mut sy, mut sxx, mut sxy, mut syy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
                for k in 1000..n {
                    let (a, b) = (y[k + lag] as f64, t[k] as f64);
                    sx += a; sy += b; sxx += a * a; sxy += a * b; syy += b * b;
                }
                let m = (n - 1000) as f64;
                let cov = sxy / m - sx * sy / (m * m);
                let vx = sxx / m - (sx / m).powi(2);
                let vy = syy / m - (sy / m).powi(2);
                let r2 = cov * cov / (vx * vy);
                best = best.max(10.0 * (r2 / (1.0 - r2)).log10());
            }
            row += &format!(" {name} {best:.1}");
        }
        println!("{row}");
    }
}

#[test]
#[ignore]
fn diag_tracker_response() {
    use crate::demod::{ChannelState, Demod};
    let fs = 13.333e6;
    for (name, kind, nat) in [("pll2.4", DemodKind::Pll, 2.4e6), ("pll6", DemodKind::Pll, 6e6), ("ekf3", DemodKind::Ekf, 3e6), ("ekf9", DemodKind::Ekf, 9e6)] {
        let mut row = format!("{name}:");
        for fm in [0.2e6, 1.0e6, 2.0e6, 3.0e6, 4.0e6] {
            let pll = PllConfig { natural_hz: nat as f32, ..PllConfig::default() };
            let ekf = EkfConfig { bandwidth_hz: nat as f32, ..EkfConfig::default() };
            let mut d = Demod::new(kind, fs, None, &LsConfig::default(), &pll, &ekf);
            let n = 40000;
            let w = std::f64::consts::TAU * fm / fs;
            let mut ph = 0.0f64;
            let (mut re, mut im, mut f) = (vec![], vec![], vec![]);
            for k in 0..n {
                let fr = 0.3 + 0.8 * (w * k as f64).sin();
                ph += fr;
                re.push(100.0 * ph.cos() as f32);
                im.push(100.0 * ph.sin() as f32);
                f.push(fr);
            }
            let mut out = vec![0.0; n];
            d.process(&re, &im, ChannelState { power: 1e4, cn: 1e4 }, &mut out);
            // Complex gain at fm relative to the true frequency.
            let (mut a, mut b) = (openvrx_core::Complex32::new(0.0, 0.0), openvrx_core::Complex32::new(0.0, 0.0));
            for k in 5000..n {
                let e = openvrx_core::Complex32::from_polar(1.0, -(w * k as f64) as f32);
                a += e * (out[k] - 0.3);
                b += e * (f[k] as f32 - 0.3);
            }
            let g = a / b;
            let resid: f64 = (5000..n).map(|k| (out[k] as f64 - f[k]).powi(2)).sum::<f64>() / (n - 5000) as f64;
            row += &format!(" {:.1}MHz |{:.2}| {:+.2}rad rms {:.3}", fm / 1e6, g.norm(), g.arg(), resid.sqrt());
        }
        println!("{row}");
    }
}
