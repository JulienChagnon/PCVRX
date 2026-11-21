// SPDX-License-Identifier: GPL-3.0-or-later
//! Records the decoded picture of view 0 as an MP4 file.
//!
//! The video thread offers every finished field to a [`VideoTap`]: when a
//! recording is on, the field's Y/U/V planes are copied onto a short queue
//! and the thread goes on (a full queue drops the field and counts it, so
//! the live picture never waits for the encoder). The encoder thread
//! converts each field to RGB with the display shader's BT.601 matrix (no
//! display adjustments, no OSD) and pipes it into an `ffmpeg` child:
//! H.264 at one frame per field (59.94/s for NTSC), 4:3, fragmented MP4 so
//! a file cut short by a crash still plays. ffmpeg must be on the PATH.
//!
//! The Learned preset's filter runs on the GPU after this point, so its
//! recordings show the picture before that filter.

use crossbeam_channel::{Receiver, Sender, TrySendError};
use openvrx_core::{Frame, VideoStandard};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Fields waiting for the encoder (about 130 ms).
const QUEUE: usize = 8;

/// One field's picture, copied out of the pipeline.
pub struct Planes {
    width: usize,
    height: usize,
    standard: Option<VideoStandard>,
    y: Vec<f32>,
    u: Vec<f32>,
    v: Vec<f32>,
}

/// The video thread's side: offered every finished field.
#[derive(Default)]
pub struct VideoTap {
    tx: Mutex<Option<Sender<Planes>>>,
    /// Fields dropped because the encoder was behind.
    pub dropped: AtomicU64,
}

impl VideoTap {
    /// Copies the frame for the encoder when a recording is on.
    pub fn offer(&self, f: &Frame) {
        let guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let Some(tx) = guard.as_ref() else { return };
        if tx.is_full() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let p = Planes {
            width: f.width,
            height: f.height,
            standard: f.meta.standard,
            y: f.y.clone(),
            u: f.u.clone(),
            v: f.v.clone(),
        };
        if let Err(TrySendError::Full(_)) = tx.try_send(p) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A running video recording.
pub struct VideoRecorder {
    tap: Arc<VideoTap>,
    thread: Option<JoinHandle<Result<u64, String>>>,
    pub path: PathBuf,
    pub started_ns: u64,
    /// Fields encoded so far.
    pub frames: Arc<AtomicU64>,
}

/// What a finished recording reports.
pub struct VideoSummary {
    pub path: PathBuf,
    pub frames: u64,
    pub dropped: u64,
    pub bytes: u64,
}

impl VideoRecorder {
    /// Starts recording what `tap` is offered into `path` (an .mp4 path
    /// that must not exist yet).
    pub fn start(tap: Arc<VideoTap>, path: PathBuf, started_ns: u64) -> Result<VideoRecorder, String> {
        let (tx, rx) = crossbeam_channel::bounded(QUEUE);
        let frames = Arc::new(AtomicU64::new(0));
        let thread = {
            let (path, frames) = (path.clone(), Arc::clone(&frames));
            std::thread::Builder::new()
                .name("openvrx-video-rec".into())
                .spawn(move || encoder_loop(rx, &path, &frames))
                .map_err(|e| format!("video recorder thread: {e}"))?
        };
        tap.dropped.store(0, Ordering::Relaxed);
        *tap.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        Ok(VideoRecorder {
            tap,
            thread: Some(thread),
            path,
            started_ns,
            frames,
        })
    }

    /// Stops the tap, lets the encoder finish the file and reports it.
    pub fn stop(mut self) -> Result<VideoSummary, String> {
        // Dropping the sender ends the encoder's queue.
        *self.tap.tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let frames = match self.thread.take().map(|t| t.join()) {
            Some(Ok(r)) => r?,
            _ => return Err("video recorder thread failed".into()),
        };
        Ok(VideoSummary {
            bytes: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
            path: self.path.clone(),
            frames,
            dropped: self.tap.dropped.load(Ordering::Relaxed),
        })
    }

    /// True while the encoder still runs (false after an ffmpeg error).
    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

struct Encoder {
    child: Child,
    stdin: ChildStdin,
    width: usize,
    height: usize,
}

/// ffmpeg arguments for raw RGB24 fields of `w` x `h` on stdin.
fn ffmpeg_args(w: usize, h: usize, standard: Option<VideoStandard>, out: &Path) -> Vec<String> {
    // One output frame per field: the decoder makes a full frame per field.
    let rate = match standard {
        Some(VideoStandard::Pal) => "50",
        _ => "60000/1001",
    };
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-n"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.extend(["-f", "rawvideo", "-pix_fmt", "rgb24"].map(String::from));
    a.extend(["-video_size".into(), format!("{w}x{h}"), "-framerate".into(), rate.into()]);
    a.extend(["-i", "pipe:0"].map(String::from));
    // x264 4:2:0 needs even sizes; BT.601 tags match the RGB conversion.
    a.extend(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"].map(String::from));
    a.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"].map(String::from));
    a.extend(["-colorspace", "smpte170m", "-color_primaries", "smpte170m", "-color_trc", "smpte170m"].map(String::from));
    a.extend(["-aspect", "4:3"].map(String::from));
    // Fragmented: every keyframe closes a playable piece.
    a.extend(["-movflags", "+frag_keyframe+empty_moov+default_base_moof", "-f", "mp4"].map(String::from));
    a.push(out.display().to_string());
    a
}

fn spawn_ffmpeg(p: &Planes, out: &Path) -> Result<Encoder, String> {
    let mut child = Command::new("ffmpeg")
        .args(ffmpeg_args(p.width, p.height, p.standard, out))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start ffmpeg ({e}); video recording needs ffmpeg on the PATH"))?;
    let stdin = child.stdin.take().ok_or("ffmpeg stdin")?;
    Ok(Encoder {
        child,
        stdin,
        width: p.width,
        height: p.height,
    })
}

/// Closes ffmpeg's input and waits for it; its error text on failure.
fn finish(mut enc: Encoder) -> Result<(), String> {
    drop(enc.stdin);
    let status = enc.child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let mut err = String::new();
    if let Some(mut s) = enc.child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    Err(format!("ffmpeg failed ({status}): {}", err.trim()))
}

/// BT.601 YUV (Y 0..1, U/V on the BT.601 scale) to RGB24, the display
/// shader's matrix without its adjustments.
fn to_rgb(p: &Planes, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(p.y.len() * 3);
    let q = |x: f32| (x * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    for ((&y, &u), &v) in p.y.iter().zip(&p.u).zip(&p.v) {
        out.push(q(y + 1.13983 * v));
        out.push(q(y - 0.39465 * u - 0.58060 * v));
        out.push(q(y + 2.03211 * u));
    }
}

fn encoder_loop(rx: Receiver<Planes>, out: &Path, frames: &AtomicU64) -> Result<u64, String> {
    let mut enc: Option<Encoder> = None;
    let mut rgb = Vec::new();
    let mut result = Ok(());
    for p in rx.iter() {
        if p.width == 0 || p.height == 0 || p.y.len() != p.width * p.height {
            continue;
        }
        if enc.is_none() {
            enc = Some(spawn_ffmpeg(&p, out)?);
        }
        let Some(e) = enc.as_mut() else { continue };
        if (e.width, e.height) != (p.width, p.height) {
            result = Err(format!(
                "picture size changed from {}x{} to {}x{} (video standard switch); recording stopped",
                e.width, e.height, p.width, p.height
            ));
            break;
        }
        to_rgb(&p, &mut rgb);
        if let Err(err) = e.stdin.write_all(&rgb) {
            result = Err(format!("writing to ffmpeg: {err}"));
            break;
        }
        frames.fetch_add(1, Ordering::Relaxed);
    }
    // The queue is drained or the sender is gone: close the file.
    let done = match enc {
        Some(e) => finish(e),
        None => Err("no picture arrived (no signal?): nothing recorded".into()),
    };
    result.and(done).map(|_| frames.load(Ordering::Relaxed))
}

/// MP4 path for a recording started at `created_utc` (same naming as raw
/// recordings: openvrx-YYYYMMDD-HHMMSS).
pub fn video_path(raw_base: &Path) -> PathBuf {
    let mut s = raw_base.as_os_str().to_owned();
    s.push(".mp4");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_and_primaries_convert_like_the_shader() {
        let p = Planes {
            width: 3,
            height: 1,
            standard: None,
            // Mid grey; pure red (V up); pure blue (U up).
            y: vec![0.5, 0.299, 0.114],
            u: vec![0.0, -0.14713 * 1.0, 0.436],
            v: vec![0.0, 0.615, -0.10001],
        };
        let mut rgb = Vec::new();
        to_rgb(&p, &mut rgb);
        assert_eq!(&rgb[0..3], &[128, 128, 128]);
        assert!(rgb[3] > 240 && rgb[4] < 15 && rgb[5] < 15, "red {:?}", &rgb[3..6]);
        assert!(rgb[6] < 15 && rgb[7] < 15 && rgb[8] > 240, "blue {:?}", &rgb[6..9]);
    }

    #[test]
    fn args_name_size_rate_and_output() {
        let a = ffmpeg_args(754, 480, Some(VideoStandard::Ntsc), Path::new("/x/rec.mp4"));
        let s = a.join(" ");
        assert!(s.contains("-video_size 754x480"));
        assert!(s.contains("-framerate 60000/1001"));
        assert!(s.contains("-c:v libx264"));
        assert!(s.ends_with("/x/rec.mp4"));
        let pal = ffmpeg_args(720, 576, Some(VideoStandard::Pal), Path::new("p.mp4")).join(" ");
        assert!(pal.contains("-framerate 50"));
    }

    /// Runs the real ffmpeg: `cargo test -p openvrx -- --ignored mp4_round_trip`.
    /// 120 fields of a red / grey / blue picture through the tap and the
    /// encoder thread; the file must report 754x480, H.264, 120 frames, and
    /// decode back to the same colours.
    #[test]
    #[ignore]
    fn mp4_round_trip_through_ffmpeg() {
        let dir = std::env::temp_dir().join(format!("openvrx-vidrec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.mp4");
        let _ = std::fs::remove_file(&path);
        let (w, h) = (754usize, 480usize);
        let mut f = Frame::new(w, h);
        f.meta.standard = Some(VideoStandard::Ntsc);
        for row in 0..h {
            for col in 0..w {
                let i = row * w + col;
                let (y, u, v) = match col * 3 / w {
                    0 => (0.299, -0.14713, 0.615),
                    1 => (0.5, 0.0, 0.0),
                    _ => (0.114, 0.436, -0.10001),
                };
                (f.y[i], f.u[i], f.v[i]) = (y, u, v);
            }
        }
        let tap = Arc::new(VideoTap::default());
        let rec = VideoRecorder::start(Arc::clone(&tap), path.clone(), 0).unwrap();
        for _ in 0..120 {
            tap.offer(&f);
            // Real time is not needed; just let the encoder keep up.
            while tap.tx.lock().unwrap().as_ref().is_some_and(|t| t.is_full()) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        let s = rec.stop().unwrap();
        assert_eq!((s.frames, s.dropped), (120, 0));
        assert!(s.bytes > 1000, "{} bytes", s.bytes);
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries"])
            .arg("stream=codec_name,width,height,nb_read_frames,display_aspect_ratio")
            .args(["-of", "csv=p=0"])
            .arg(&path)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&probe.stdout).trim().to_string();
        assert_eq!(out, "h264,754,480,4:3,120", "ffprobe: {out}");
        // One decoded frame back to RGB: the three bands keep their colour.
        let raw = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(raw.len(), w * h * 3);
        let px = |col: usize| {
            let i = ((h / 2) * w + col) * 3;
            [raw[i], raw[i + 1], raw[i + 2]]
        };
        let (r, g, b) = (px(w / 6), px(w / 2), px(5 * w / 6));
        assert!(r[0] > 220 && r[1] < 40 && r[2] < 40, "red band {r:?}");
        assert!(g.iter().all(|&c| (115..=140).contains(&c)), "grey band {g:?}");
        assert!(b[0] < 40 && b[1] < 40 && b[2] > 220, "blue band {b:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_path_appends_mp4() {
        assert_eq!(video_path(Path::new("rec/openvrx-20260101-120000")), PathBuf::from("rec/openvrx-20260101-120000.mp4"));
    }
}
