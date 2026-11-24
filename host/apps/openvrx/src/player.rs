// SPDX-License-Identifier: GPL-3.0-or-later
//! The recordings page's video player. `ffprobe` reads the size, frame
//! rate, display aspect and duration; an `ffmpeg` child decodes to RGBA on a
//! pipe and a reader thread hands whole frames over a 4-frame queue (when the
//! UI does not take them, ffmpeg blocks on the pipe: no unbounded memory).
//! The UI thread shows the newest frame that is due on the wall clock and
//! uploads it to one egui texture.
//!
//! Seeking restarts ffmpeg at the new time (`-ss` before `-i`: a fast seek
//! that still decodes from the keyframe, so it lands on the exact frame).
//! While the scrubber is dragged, seeks are spaced 150 ms apart. Decoding is
//! capped at 2 threads and runs only while the page shows a video, so it
//! takes little CPU from the live picture; frames larger than 960 pixels wide
//! are scaled down by ffmpeg.

use egui::{Color32, ColorImage, Rect, TextureHandle, TextureOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const QUEUE: usize = 4;
const MAX_W: usize = 960;
const SCRUB_SEEK: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, PartialEq)]
pub struct VideoInfo {
    /// Decoded (possibly scaled) size.
    pub w: usize,
    pub h: usize,
    pub fps: f64,
    /// Width / height as shown.
    pub aspect: f32,
    pub duration_s: Option<f64>,
}

struct Frame {
    pts_s: f64,
    rgba: Vec<u8>,
}

enum Msg {
    Info(VideoInfo),
    Frame(Frame),
    End,
    Error(String),
}

struct Decoder {
    rx: Receiver<Msg>,
    child: Arc<Mutex<Option<Child>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut c) = self.child.lock() {
            if let Some(c) = c.as_mut() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

#[derive(Default)]
pub struct Player {
    pub path: Option<PathBuf>,
    pub info: Option<VideoInfo>,
    dec: Option<Decoder>,
    texture: Option<TextureHandle>,
    pub playing: bool,
    /// Media time of the frame on screen.
    pub shown_s: f64,
    /// Wall time and media time playback (re)started at.
    anchor: Option<(Instant, f64)>,
    /// The next decoded frame, not yet due.
    pending: Option<Frame>,
    /// Show the next frame as soon as it arrives (after open or seek).
    want_first: bool,
    pub ended: bool,
    pub error: Option<String>,
    last_seek: Option<Instant>,
}

impl Player {
    /// Opens `path` paused on its first frame.
    pub fn open(&mut self, path: &Path) {
        self.close();
        self.path = Some(path.to_path_buf());
        self.start_decoder(0.0);
    }

    pub fn close(&mut self) {
        self.dec = None;
        self.path = None;
        self.info = None;
        self.playing = false;
        self.anchor = None;
        self.pending = None;
        self.shown_s = 0.0;
        self.ended = false;
        self.error = None;
    }

    /// Stops decoding but keeps the file and the frame on screen (the page
    /// was closed); `resume` picks up where it was.
    pub fn suspend(&mut self) {
        self.pause();
        self.dec = None;
        self.pending = None;
    }

    pub fn is_open(&self) -> bool {
        self.path.is_some()
    }

    fn start_decoder(&mut self, at_s: f64) {
        let Some(path) = self.path.clone() else { return };
        self.pending = None;
        self.want_first = true;
        self.ended = false;
        self.error = None;
        let (tx, rx) = sync_channel(QUEUE);
        let child = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (c2, s2, info) = (Arc::clone(&child), Arc::clone(&stop), self.info.clone());
        let spawned = std::thread::Builder::new()
            .name("player-decode".into())
            .spawn(move || decode_thread(&path, at_s, info, &tx, &c2, &s2));
        if let Err(e) = spawned {
            self.error = Some(format!("decoder thread: {e}"));
            return;
        }
        self.dec = Some(Decoder { rx, child, stop });
    }

    pub fn duration(&self) -> Option<f64> {
        self.info.as_ref().and_then(|i| i.duration_s)
    }

    fn frame_s(&self) -> f64 {
        1.0 / self.info.as_ref().map_or(30.0, |i| i.fps.max(1.0))
    }

    pub fn play(&mut self) {
        if self.path.is_none() {
            return;
        }
        if self.ended || self.dec.is_none() {
            let from = if self.ended { 0.0 } else { self.shown_s };
            self.start_decoder(from);
            self.shown_s = from;
        }
        self.playing = true;
        self.anchor = Some((Instant::now(), self.shown_s));
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.anchor = None;
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.pause()
        } else {
            self.play()
        }
    }

    /// Jumps to `t` seconds. `scrubbing`: part of a drag, so spaced out.
    pub fn seek(&mut self, t: f64, scrubbing: bool) {
        if self.path.is_none() {
            return;
        }
        if scrubbing && self.last_seek.is_some_and(|l| l.elapsed() < SCRUB_SEEK) {
            return;
        }
        let max = self.duration().unwrap_or(f64::MAX);
        let t = t.clamp(0.0, (max - self.frame_s()).max(0.0));
        self.last_seek = Some(Instant::now());
        self.start_decoder(t);
        self.shown_s = t;
        if self.playing {
            self.anchor = Some((Instant::now(), t));
        }
    }

    /// One frame forward (paused) or back.
    pub fn step(&mut self, forward: bool) {
        self.pause();
        if forward {
            if self.dec.is_none() {
                self.start_decoder(self.shown_s);
            }
            self.want_first = true;
        } else {
            let t = self.shown_s - self.frame_s();
            self.seek(t, false);
        }
    }

    /// Takes decoded frames and uploads the one due now. Call every frame
    /// the page is shown; returns when the next frame is due.
    pub fn update(&mut self, ctx: &egui::Context) -> Option<Duration> {
        let now_media = match (self.playing, self.anchor) {
            (true, Some((t0, m0))) => m0 + t0.elapsed().as_secs_f64(),
            _ => self.shown_s,
        };
        let mut show: Option<Frame> = None;
        loop {
            let f = match self.pending.take() {
                Some(f) => Some(f),
                None => match self.dec.as_ref().map(|d| d.rx.try_recv()) {
                    Some(Ok(Msg::Frame(f))) => Some(f),
                    Some(Ok(Msg::Info(i))) => {
                        self.info = Some(i);
                        continue;
                    }
                    Some(Ok(Msg::End)) | Some(Err(TryRecvError::Disconnected)) => {
                        self.dec = None;
                        if show.is_none() && self.playing {
                            self.ended = true;
                            self.pause();
                        }
                        None
                    }
                    Some(Ok(Msg::Error(e))) => {
                        self.error = Some(e);
                        self.dec = None;
                        self.pause();
                        None
                    }
                    Some(Err(TryRecvError::Empty)) | None => None,
                },
            };
            let Some(f) = f else { break };
            let due = self.want_first || (self.playing && f.pts_s <= now_media + 0.004);
            if due {
                self.want_first = false;
                show = Some(f);
                if !self.playing {
                    break;
                }
            } else {
                self.pending = Some(f);
                break;
            }
        }
        if let (Some(f), Some(info)) = (show, self.info.as_ref()) {
            self.shown_s = f.pts_s;
            let img = ColorImage::from_rgba_unmultiplied([info.w, info.h], &f.rgba);
            match self.texture.as_mut() {
                Some(t) if t.size() == [info.w, info.h] => t.set(img, TextureOptions::LINEAR),
                _ => self.texture = Some(ctx.load_texture("openvrx-player", img, TextureOptions::LINEAR)),
            }
        }
        if !self.playing {
            return None;
        }
        Some(match &self.pending {
            Some(f) => Duration::from_secs_f64((f.pts_s - now_media).max(0.0)),
            None => Duration::from_millis(4),
        })
    }

    /// Paints the frame on screen into `area`, letterboxed to its aspect.
    pub fn paint(&self, p: &egui::Painter, area: Rect) {
        p.rect_filled(area, 0.0, Color32::BLACK);
        let (Some(t), Some(info)) = (self.texture.as_ref(), self.info.as_ref()) else { return };
        let a = info.aspect.max(0.1);
        let (w, h) = if area.width() / area.height() > a {
            (area.height() * a, area.height())
        } else {
            (area.width(), area.width() / a)
        };
        let r = Rect::from_center_size(area.center(), egui::vec2(w, h));
        let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(t.id(), r, uv, Color32::WHITE);
    }

    pub fn has_frame(&self) -> bool {
        self.texture.is_some() && self.path.is_some()
    }
}

/// "1:02.5" for the player's clock.
pub fn fmt_time(s: f64) -> String {
    // Round to the tenth first, so 59.97 reads 1:00.0, not 0:60.0.
    let s = (s.max(0.0) * 10.0).round() / 10.0;
    let m = (s / 60.0).floor() as u64;
    format!("{m}:{:04.1}", s - m as f64 * 60.0)
}

/// "60000/1001" or "50" as a rate.
fn parse_rate(s: &str) -> Option<f64> {
    match s.split_once('/') {
        Some((a, b)) => {
            let (a, b) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
            (b > 0.0 && a > 0.0).then(|| a / b)
        }
        None => s.trim().parse().ok().filter(|v: &f64| *v > 0.0),
    }
}

/// "4:3" as 1.333.
fn parse_ratio(s: &str) -> Option<f32> {
    let (a, b) = s.split_once(':')?;
    let (a, b) = (a.parse::<f32>().ok()?, b.parse::<f32>().ok()?);
    (a > 0.0 && b > 0.0).then(|| a / b)
}

/// What ffprobe's JSON says about the first video stream.
pub fn parse_probe(json: &str) -> Result<VideoInfo, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("ffprobe: {e}"))?;
    let st = v["streams"].get(0).ok_or("no video stream")?;
    let w = st["width"].as_u64().ok_or("no width")? as usize;
    let h = st["height"].as_u64().ok_or("no height")? as usize;
    let fps = st["avg_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .or_else(|| st["r_frame_rate"].as_str().and_then(parse_rate))
        .unwrap_or(30.0);
    let aspect = st["display_aspect_ratio"]
        .as_str()
        .and_then(parse_ratio)
        .unwrap_or(w as f32 / h.max(1) as f32);
    let duration_s = v["format"]["duration"].as_str().and_then(|d| d.parse().ok());
    let (w, h) = if w > MAX_W { (MAX_W, ((h * MAX_W / w) + 1) & !1) } else { (w, h) };
    Ok(VideoInfo { w, h, fps, aspect, duration_s })
}

fn probe(path: &Path) -> Result<VideoInfo, String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height,avg_frame_rate,r_frame_rate,display_aspect_ratio:format=duration")
        .args(["-of", "json"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ffprobe ({e}); playback needs ffmpeg on the PATH"))?;
    if !out.status.success() {
        return Err(format!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

fn decode_thread(
    path: &Path,
    at_s: f64,
    info: Option<VideoInfo>,
    tx: &SyncSender<Msg>,
    child: &Mutex<Option<Child>>,
    stop: &AtomicBool,
) {
    let info = match info.map(Ok).unwrap_or_else(|| probe(path)) {
        Ok(i) => i,
        Err(e) => {
            let _ = tx.send(Msg::Error(e));
            return;
        }
    };
    let _ = tx.send(Msg::Info(info.clone()));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "2"])
        .args(["-ss", &format!("{at_s:.3}")])
        .arg("-i")
        .arg(path)
        .args(["-an", "-sn", "-vf", &format!("scale={}:{}", info.w, info.h)])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut c = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Msg::Error(format!("cannot run ffmpeg ({e}); playback needs ffmpeg on the PATH")));
            return;
        }
    };
    let mut out = c.stdout.take().expect("piped");
    let mut err = c.stderr.take().expect("piped");
    if let Ok(mut g) = child.lock() {
        *g = Some(c);
    }
    let size = info.w * info.h * 4;
    let mut n = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut rgba = vec![0u8; size];
        if out.read_exact(&mut rgba).is_err() {
            break;
        }
        let pts_s = at_s + n as f64 / info.fps;
        n += 1;
        if tx.send(Msg::Frame(Frame { pts_s, rgba })).is_err() {
            return;
        }
    }
    let mut text = String::new();
    let _ = err.read_to_string(&mut text);
    let failed = n == 0 && !text.trim().is_empty();
    let _ = tx.send(if failed { Msg::Error(format!("ffmpeg: {}", text.trim())) } else { Msg::End });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_json() {
        let j = r#"{"streams":[{"width":754,"height":480,"avg_frame_rate":"60000/1001","r_frame_rate":"60000/1001","display_aspect_ratio":"4:3"}],"format":{"duration":"30.497133"}}"#;
        let i = parse_probe(j).unwrap();
        assert_eq!((i.w, i.h), (754, 480));
        assert!((i.fps - 59.94).abs() < 0.01);
        assert!((i.aspect - 4.0 / 3.0).abs() < 1e-4);
        assert_eq!(i.duration_s, Some(30.497133));
        let big = r#"{"streams":[{"width":1920,"height":1080,"avg_frame_rate":"0/0","r_frame_rate":"30/1"}],"format":{}}"#;
        let i = parse_probe(big).unwrap();
        assert_eq!((i.w, i.h), (960, 540));
        assert_eq!(i.fps, 30.0);
        assert_eq!(i.duration_s, None);
        assert!(parse_probe(r#"{"streams":[]}"#).is_err());
    }

    #[test]
    fn clock_text() {
        assert_eq!(fmt_time(0.0), "0:00.0");
        assert_eq!(fmt_time(62.54), "1:02.5");
        assert_eq!(fmt_time(59.97), "1:00.0");
    }
}
