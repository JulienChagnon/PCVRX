// SPDX-License-Identifier: GPL-3.0-or-later
//! The recordings page (key B): the recording folder as a list, a video
//! player, what each raw recording's sidecar says, and the external tools
//! (`tools.rs`) that can run on the selection, with their jobs.
//!
//! The folder is read on a background thread (it may be a slow USB drive),
//! one level deep. A raw recording (`NAME.openvrx.json` and its files) is one
//! entry; every video whose name starts with `NAME` (the app's `NAME.mp4`,
//! a tool's `NAME-clean.mp4`...) belongs to it. Other videos are entries of
//! their own. Nothing here deletes or moves a file.
//!
//! The live picture keeps running while the page is open (recording too);
//! only its drawing is skipped. Keys while open: Space play/pause, Left/Right
//! 5 s back/forward, `,` and `.` one frame, Home start, Up/Down previous/next
//! recording, Enter play; Esc or B back to the live picture.

use crate::player::{fmt_time, Player};
use crate::theme::{self, ACCENT, ACCENT_SOFT, BG, DIM, FAINT, HOVER, LINE, RAISED, TEXT};
use crate::tools::{InputKind, JobState, Jobs, ToolSpec};
use egui::{Align2, Color32, Key, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

const VIDEO_EXT: [&str; 6] = ["mp4", "mkv", "webm", "mov", "avi", "m4v"];

#[derive(Clone, Debug, PartialEq)]
pub struct VideoFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

/// What a raw recording's sidecar says (lenient: missing fields stay empty).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawInfo {
    pub sidecar: PathBuf,
    pub created_utc: String,
    pub source: String,
    pub channel_mhz: Option<u32>,
    pub receivers: usize,
    pub lanes: String,
    pub divider: Option<u64>,
    pub duration_s: Option<f64>,
    pub closed_cleanly: Option<bool>,
    pub picture_mode: Option<String>,
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Base name of a raw recording, or a lone video's file name.
    pub name: String,
    pub raw: Option<RawInfo>,
    pub videos: Vec<VideoFile>,
    /// Bytes of every file of the entry.
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

/// One file of the folder listing.
#[derive(Clone, Debug)]
pub struct Listed {
    pub name: String,
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

fn is_video(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, e)| VIDEO_EXT.iter().any(|v| e.eq_ignore_ascii_case(v)))
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// Groups a folder listing into entries (sidecars not read yet: `raw` has
/// only its path). Newest first.
pub fn group(folder: &Path, files: &[Listed]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = files
        .iter()
        .filter_map(|f| f.name.strip_suffix(".openvrx.json").map(|b| (b, f)))
        .map(|(base, f)| Entry {
            name: base.to_string(),
            raw: Some(RawInfo { sidecar: folder.join(&f.name), ..RawInfo::default() }),
            videos: Vec::new(),
            bytes: 0,
            modified: f.modified,
        })
        .collect();
    // Longest base first, so "a-2" wins over "a" for "a-2.mp4".
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&k| std::cmp::Reverse(entries[k].name.len()));
    let mut lone = Vec::new();
    for f in files {
        let video = is_video(&f.name);
        let owner = order.iter().copied().find(|&k| {
            let b = &entries[k].name;
            if video {
                // "NAME.mp4", "NAME-clean.mp4", but not "NAME0.mp4" (another recording).
                let s = stem(&f.name);
                s.starts_with(b.as_str()) && s[b.len()..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric())
            } else {
                f.name.len() > b.len() && f.name.starts_with(b.as_str()) && f.name.as_bytes()[b.len()] == b'.'
            }
        });
        match owner {
            Some(k) => {
                let e = &mut entries[k];
                e.bytes += f.bytes;
                e.modified = e.modified.max(f.modified);
                if video {
                    e.videos.push(VideoFile { path: folder.join(&f.name), bytes: f.bytes, modified: f.modified });
                }
            }
            None if video => lone.push(Entry {
                name: f.name.clone(),
                raw: None,
                videos: vec![VideoFile { path: folder.join(&f.name), bytes: f.bytes, modified: f.modified }],
                bytes: f.bytes,
                modified: f.modified,
            }),
            None => {}
        }
    }
    entries.extend(lone);
    for e in &mut entries {
        e.videos.sort_by(|a, b| a.path.cmp(&b.path));
    }
    entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| b.name.cmp(&a.name)));
    entries
}

/// Reads the fields the page shows from a sidecar's JSON.
pub fn parse_sidecar(text: &str, sidecar: &Path) -> RawInfo {
    let v: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
    let rx = v["receivers"].as_array().cloned().unwrap_or_default();
    let r0 = rx.first().cloned().unwrap_or_default();
    let ctx = &v["context"];
    let duration_s = ctx["summary"]["duration_s"].as_f64().or_else(|| {
        // Not closed: estimate from rx0's bytes, rate and word size.
        let bytes = r0["bytes"].as_f64()?;
        let rate = r0["sample_rate_hz"].as_f64()?;
        let word = r0["sample_format"]["lanes"]["word_bytes"].as_f64().unwrap_or(2.0);
        (rate > 0.0 && bytes > 0.0).then(|| bytes / (rate * word))
    });
    RawInfo {
        sidecar: sidecar.to_path_buf(),
        created_utc: v["created_utc"].as_str().unwrap_or_default().to_string(),
        source: v["source"].as_str().unwrap_or_default().to_string(),
        channel_mhz: r0["channel_mhz"].as_u64().map(|m| m as u32),
        receivers: rx.len(),
        lanes: r0["sample_format"]["preset"].as_str().unwrap_or_default().to_string(),
        divider: r0["sample_format"]["divider"].as_u64(),
        duration_s,
        closed_cleanly: v["closed_cleanly"].as_bool(),
        picture_mode: ctx["app"]["picture_mode"].as_str().map(str::to_string),
        notes: v["notes"].as_str().unwrap_or_default().to_string(),
    }
}

fn scan(folder: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let files: Vec<Listed> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            md.is_file().then(|| Listed {
                name: e.file_name().to_string_lossy().into_owned(),
                bytes: md.len(),
                modified: md.modified().ok(),
            })
        })
        .collect();
    let mut entries = group(folder, &files);
    for e in &mut entries {
        if let Some(r) = e.raw.as_mut() {
            if let Ok(text) = std::fs::read_to_string(&r.sidecar) {
                *r = parse_sidecar(&text, &r.sidecar);
            }
        }
    }
    Ok(entries)
}

/// "2026-11-04 21:05 UTC" from a time (no time zone database here).
pub fn fmt_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to civil date (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, (rem / 60) % 60)
}

/// "2026-10-29T21:14:03Z" as "2026-10-29 21:14 UTC".
fn fmt_created(s: &str) -> String {
    match (s.get(0..10), s.get(11..16)) {
        (Some(d), Some(t)) => format!("{d} {t} UTC"),
        _ => s.to_string(),
    }
}

/// Opens a file or folder with the desktop's default program.
pub fn open_external(path: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        c.arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    // Reap it so it does not linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The live state shown in the page's header.
pub struct LiveLine {
    pub channel: String,
    pub lock: (&'static str, Color32),
    pub recording: Vec<String>,
    pub mode: (&'static str, Color32),
}

pub struct Library {
    pub open: bool,
    folder: PathBuf,
    entries: Vec<Entry>,
    scanning: Option<Receiver<Result<Vec<Entry>, String>>>,
    scan_error: Option<String>,
    /// Name of the selected entry.
    selected: Option<String>,
    pub player: Player,
    search: String,
    pub jobs: Jobs,
    tools_dir: PathBuf,
    tools: Vec<ToolSpec>,
    tool_notes: Vec<String>,
    /// Job whose full log is shown.
    log_open: Option<u64>,
    /// Messages for the app's OSD (tool started, failed...).
    notes: Vec<String>,
}

impl Library {
    pub fn new(tools_dir: PathBuf) -> Library {
        Library {
            open: false,
            folder: PathBuf::new(),
            entries: Vec::new(),
            scanning: None,
            scan_error: None,
            selected: None,
            player: Player::default(),
            search: String::new(),
            jobs: Jobs::default(),
            tools_dir,
            tools: Vec::new(),
            tool_notes: Vec::new(),
            log_open: None,
            notes: Vec::new(),
        }
    }

    pub fn toggle(&mut self, folder: &Path) {
        self.open = !self.open;
        if self.open {
            if folder != self.folder || self.entries.is_empty() {
                self.folder = folder.to_path_buf();
                self.rescan();
            }
            (self.tools, self.tool_notes) = crate::tools::load_tools(&self.tools_dir);
        } else {
            self.player.suspend();
        }
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.player.suspend();
        }
    }

    fn rescan(&mut self) {
        let (tx, rx) = mpsc::channel();
        let folder = self.folder.clone();
        let ok = std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || {
                let _ = tx.send(scan(&folder));
            })
            .is_ok();
        self.scanning = ok.then_some(rx);
    }

    /// Jobs and scans; call every frame, open or not. Returns OSD messages.
    pub fn poll(&mut self) -> Vec<String> {
        if let Some(rx) = &self.scanning {
            if let Ok(r) = rx.try_recv() {
                self.scanning = None;
                match r {
                    Ok(e) => {
                        self.entries = e;
                        self.scan_error = None;
                    }
                    Err(e) => self.scan_error = Some(e),
                }
            }
        }
        let before: Vec<(u64, JobState)> = self.jobs.list.iter().map(|j| (j.id, j.state.clone())).collect();
        let done = self.jobs.poll();
        for j in &self.jobs.list {
            let was_running = before.iter().any(|(id, s)| *id == j.id && *s == JobState::Running);
            if !was_running {
                continue;
            }
            match &j.state {
                JobState::Done => self.notes.push(format!("{} finished", j.tool)),
                JobState::Failed(e) => self.notes.push(format!("error: {} failed ({e})", j.tool)),
                _ => {}
            }
        }
        if !done.is_empty() && !self.folder.as_os_str().is_empty() {
            self.rescan();
        }
        std::mem::take(&mut self.notes)
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        (0..self.entries.len())
            .filter(|&k| q.is_empty() || self.entries[k].name.to_lowercase().contains(&q))
            .collect()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let s = self.selected.as_ref()?;
        self.entries.iter().find(|e| &e.name == s)
    }

    fn select(&mut self, k: usize) {
        let Some(e) = self.entries.get(k) else { return };
        if self.selected.as_ref() == Some(&e.name) {
            return;
        }
        self.selected = Some(e.name.clone());
        match e.videos.first().map(|v| v.path.clone()) {
            Some(p) => self.player.open(&p),
            None => self.player.close(),
        }
    }

    /// Keys of the page; true when the key was used (the app's own binding
    /// for it is then skipped).
    pub fn handle_key(&mut self, key: Key) -> bool {
        if !self.open {
            return false;
        }
        match key {
            Key::Space => self.player.toggle(),
            Key::ArrowLeft => self.player.seek(self.player.shown_s - 5.0, false),
            Key::ArrowRight => self.player.seek(self.player.shown_s + 5.0, false),
            Key::Comma => self.player.step(false),
            Key::Period => self.player.step(true),
            Key::Home => self.player.seek(0.0, false),
            Key::Enter => self.player.play(),
            Key::ArrowUp | Key::ArrowDown => {
                let vis = self.visible();
                let cur = self.selected.as_ref().and_then(|s| vis.iter().position(|&k| &self.entries[k].name == s));
                let next = match (cur, key == Key::ArrowDown) {
                    (None, _) => 0,
                    (Some(c), true) => (c + 1).min(vis.len().saturating_sub(1)),
                    (Some(c), false) => c.saturating_sub(1),
                };
                if let Some(&k) = vis.get(next) {
                    self.select(k);
                }
            }
            _ => return false,
        }
        true
    }

    /// Draws the page into the window (in place of the live picture).
    pub fn show(&mut self, ui: &mut egui::Ui, live: &LiveLine) {
        let ctx = ui.ctx().clone();
        if let Some(wait) = self.player.update(&ctx) {
            ctx.request_repaint_after(wait);
        }
        if self.scanning.is_some() || self.jobs.running() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        egui::Panel::top("library-head")
            .frame(egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::symmetric(16, 10)))
            .show(ui, |ui| self.header(ui, live));
        if !self.jobs.list.is_empty() {
            egui::Panel::bottom("library-jobs")
                .resizable(true)
                .default_size(130.0)
                .frame(egui::Frame::NONE.fill(theme::SURFACE).inner_margin(egui::Margin::symmetric(16, 10)))
                .show(ui, |ui| self.jobs_ui(ui));
        }
        egui::Panel::left("library-list")
            .resizable(true)
            .default_size(360.0)
            .min_size(260.0)
            .frame(egui::Frame::NONE.fill(theme::SURFACE).inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| self.list_ui(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| self.detail_ui(ui));
    }

    fn header(&mut self, ui: &mut egui::Ui, live: &LiveLine) {
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("\u{2190}  Live").color(TEXT)).corner_radius(theme::R)).on_hover_text("Back to the live picture (Esc or B)").clicked() {
                self.close();
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Recordings").size(theme::SIZE_TITLE).color(TEXT));
            ui.add_space(6.0);
            ui.label(RichText::new(self.folder.display().to_string()).size(theme::SIZE_SMALL).color(FAINT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for r in live.recording.iter().rev() {
                    ui.label(RichText::new(r).font(theme::mono(12.5)).color(theme::BAD));
                }
                ui.label(RichText::new(live.lock.0).font(theme::mono(12.5)).color(live.lock.1));
                ui.label(RichText::new(&live.channel).font(theme::mono(12.5)).color(TEXT));
                ui.label(RichText::new(live.mode.0.to_uppercase()).font(theme::mono(12.5)).color(live.mode.1));
                ui.label(RichText::new("LIVE").font(theme::mono(11.0)).color(FAINT));
            });
        });
    }

    fn list_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let w = ui.available_width() - 140.0;
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Filter").desired_width(w));
            let busy = self.scanning.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Refresh")).on_hover_text("Read the folder again").clicked() {
                self.rescan();
            }
            if ui.button("Folder").on_hover_text("Open the folder in the file manager").clicked() {
                if let Err(e) = open_external(&self.folder) {
                    self.notes.push(format!("error: cannot open the folder: {e}"));
                }
            }
        });
        ui.add_space(6.0);
        if let Some(e) = &self.scan_error {
            ui.label(RichText::new(e).color(theme::BAD));
        }
        let vis = self.visible();
        let raws = vis.iter().filter(|&&k| self.entries[k].raw.is_some()).count();
        let status = if self.scanning.is_some() && self.entries.is_empty() {
            "reading the folder...".to_string()
        } else {
            format!("{} recordings, {} videos only", raws, vis.len() - raws)
        };
        ui.label(RichText::new(status).size(theme::SIZE_SMALL).color(FAINT));
        ui.add_space(4.0);
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for &k in &vis {
                let e = &self.entries[k];
                let on = self.selected.as_ref() == Some(&e.name);
                if entry_row(ui, e, on).clicked() {
                    clicked = Some(k);
                }
            }
        });
        if let Some(k) = clicked {
            self.select(k);
        }
    }

    fn detail_ui(&mut self, ui: &mut egui::Ui) {
        let Some(e) = self.selected_entry().cloned() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Pick a recording on the left (Up/Down)").color(FAINT));
            });
            return;
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new(&e.name).size(theme::SIZE_TITLE).color(TEXT));
            ui.add_space(8.0);
            self.player_ui(ui, &e);
            ui.add_space(14.0);
            ui.columns(2, |cols| {
                self.files_ui(&mut cols[0], &e);
                if let Some(r) = &e.raw {
                    info_ui(&mut cols[1], r, e.bytes);
                }
            });
            ui.add_space(14.0);
            self.tools_ui(ui, &e);
        });
    }

    fn player_ui(&mut self, ui: &mut egui::Ui, e: &Entry) {
        let w = ui.available_width();
        let h = (w * 3.0 / 4.0).min(ui.ctx().content_rect().height() * 0.52).max(160.0);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
        let p = ui.painter();
        if self.player.is_open() {
            self.player.paint(p, rect);
            if let Some(err) = &self.player.error {
                p.text(rect.center(), Align2::CENTER_CENTER, err, theme::body(), theme::BAD);
            } else if !self.player.has_frame() {
                p.text(rect.center(), Align2::CENTER_CENTER, "loading...", theme::body(), DIM);
            }
            if resp.clicked() {
                self.player.toggle();
            }
        } else {
            p.rect_filled(rect, theme::R, theme::SURFACE);
            let msg = if e.raw.is_some() {
                "No video of this recording yet. Make one with a tool below, or record video (M) next time."
            } else {
                "No video"
            };
            p.text(rect.center(), Align2::CENTER_CENTER, msg, theme::body(), FAINT);
            return;
        }
        ui.add_space(6.0);
        // Transport: play, frame steps, time, scrubber.
        ui.horizontal(|ui| {
            let label = if self.player.playing { "Pause" } else { "Play" };
            if ui.add(theme::primary_button(label).min_size(Vec2::new(64.0, 26.0))).on_hover_text("Space").clicked() {
                self.player.toggle();
            }
            if ui.button("-1 f").on_hover_text("One frame back (,)").clicked() {
                self.player.step(false);
            }
            if ui.button("+1 f").on_hover_text("One frame forward (.)").clicked() {
                self.player.step(true);
            }
            let dur = self.player.duration().unwrap_or(0.0);
            let time = format!("{} / {}", fmt_time(self.player.shown_s), fmt_time(dur));
            ui.label(RichText::new(time).font(theme::mono(12.5)).color(DIM));
            let w = ui.available_width() - 84.0;
            if let Some(t) = scrubber(ui, w.max(60.0), self.player.shown_s, dur) {
                self.player.seek(t.0, t.1);
            }
            if ui.button("Open").on_hover_text("Open in the desktop's video player").clicked() {
                if let Some(p) = self.player.path.clone() {
                    if let Err(err) = open_external(&p) {
                        self.notes.push(format!("error: cannot open {}: {err}", p.display()));
                    }
                }
            }
        });
    }

    fn files_ui(&mut self, ui: &mut egui::Ui, e: &Entry) {
        theme::section(ui, "Videos");
        if e.videos.is_empty() {
            ui.label(RichText::new("none").color(FAINT));
        }
        let mut play = None;
        for v in &e.videos {
            let on = self.player.path.as_deref() == Some(v.path.as_path());
            let name = v.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
            let p = ui.painter();
            if on {
                p.rect_filled(rect, theme::R, ACCENT_SOFT);
                p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), 0.0, ACCENT);
            } else if r.hovered() {
                p.rect_filled(rect, theme::R, HOVER);
            }
            p.text(rect.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, &name, egui::FontId::proportional(13.0), TEXT);
            p.text(
                rect.right_center() - Vec2::new(8.0, 0.0),
                Align2::RIGHT_CENTER,
                crate::osd::fmt_bytes(v.bytes),
                theme::mono(11.5),
                FAINT,
            );
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                play = Some(v.path.clone());
            }
        }
        if let Some(p) = play {
            self.player.open(&p);
        }
    }

    fn tools_ui(&mut self, ui: &mut egui::Ui, e: &Entry) {
        theme::section(ui, "Tools");
        let video = self.player.path.clone();
        let usable: Vec<(ToolSpec, PathBuf)> = self
            .tools
            .iter()
            .filter_map(|t| match t.input {
                InputKind::Raw => e.raw.as_ref().map(|r| (t.clone(), r.sidecar.clone())),
                InputKind::Video => video.clone().map(|v| (t.clone(), v)),
            })
            .collect();
        if self.tools.is_empty() {
            ui.label(
                RichText::new(format!(
                    "No tools set up. Put a tools file in {} (see docs/USAGE.md, \"Recordings page\").",
                    self.tools_dir.display()
                ))
                .color(FAINT),
            );
        } else if usable.is_empty() {
            ui.label(RichText::new("No tool takes this kind of file.").color(FAINT));
        }
        ui.horizontal_wrapped(|ui| {
            for (t, input) in &usable {
                let target = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let tip = if t.description.is_empty() {
                    format!("on {target}")
                } else {
                    format!("{}\non {target}", t.description)
                };
                if ui.add(theme::primary_button(&t.name).min_size(Vec2::new(0.0, 28.0))).on_hover_text(tip).clicked() {
                    match self.jobs.start(t, input) {
                        Ok(_) => self.notes.push(format!("{} started on {target}", t.name)),
                        Err(err) => self.notes.push(format!("error: {err}")),
                    }
                }
            }
        });
        for n in &self.tool_notes {
            ui.label(RichText::new(n).size(theme::SIZE_SMALL).color(theme::WARN));
        }
    }

    fn jobs_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            theme::section(ui, &format!("Jobs ({} running)", self.jobs.running()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear finished").clicked() {
                    self.jobs.clear_finished();
                }
            });
        });
        let mut cancel = None;
        let mut play = None;
        let mut toggle_log = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for j in self.jobs.list.iter().rev() {
                ui.horizontal(|ui| {
                    let (text, color) = match &j.state {
                        JobState::Running => ("RUNNING", ACCENT),
                        JobState::Done => ("DONE", theme::GOOD),
                        JobState::Failed(_) => ("FAILED", theme::BAD),
                        JobState::Cancelled => ("STOPPED", FAINT),
                    };
                    ui.add_sized([70.0, 20.0], egui::Label::new(RichText::new(text).font(theme::mono(11.5)).color(color)));
                    let input = j.input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new(format!("{}  ", j.tool)).color(TEXT));
                    ui.label(RichText::new(input).size(theme::SIZE_SMALL).color(FAINT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(crate::osd::fmt_elapsed(j.elapsed().as_nanos() as u64)).font(theme::mono(11.5)).color(DIM));
                        if j.running() && ui.small_button("Stop").clicked() {
                            cancel = Some(j.id);
                        }
                        if j.state == JobState::Done {
                            if let Some(o) = j.output.as_ref().filter(|o| o.exists()) {
                                if ui.small_button("Play").clicked() {
                                    play = Some(o.clone());
                                }
                            }
                        }
                        let log_label = if self.log_open == Some(j.id) { "Hide log" } else { "Log" };
                        if ui.small_button(log_label).clicked() {
                            toggle_log = Some(j.id);
                        }
                        if j.running() {
                            let w = (ui.available_width() - 8.0).clamp(60.0, 260.0);
                            match j.progress() {
                                Some(f) => {
                                    ui.add(egui::ProgressBar::new(f).desired_width(w).show_percentage());
                                }
                                None => {
                                    ui.add(egui::Spinner::new());
                                }
                            }
                        }
                    });
                });
                if j.running() || matches!(j.state, JobState::Failed(_)) {
                    if let Some(l) = j.last_line() {
                        ui.label(RichText::new(l).font(theme::mono(11.0)).color(FAINT));
                    }
                }
                if self.log_open == Some(j.id) {
                    egui::Frame::NONE.fill(BG).inner_margin(egui::Margin::same(8)).corner_radius(theme::R).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for l in j.log_lines() {
                            ui.label(RichText::new(l).font(theme::mono(11.0)).color(DIM));
                        }
                    });
                }
                ui.add_space(4.0);
            }
        });
        if let Some(id) = cancel {
            self.jobs.cancel(id);
        }
        if let Some(id) = toggle_log {
            self.log_open = if self.log_open == Some(id) { None } else { Some(id) };
        }
        if let Some(p) = play {
            self.player.open(&p);
            self.player.play();
        }
    }
}

/// One entry in the list: name, when, how long, size and what it holds.
fn entry_row(ui: &mut egui::Ui, e: &Entry, on: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(rect, theme::R, ACCENT_SOFT);
        p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), 0.0, ACCENT);
    } else if r.hovered() {
        p.rect_filled(rect, theme::R, HOVER);
    }
    let left = rect.left() + 12.0;
    let top = rect.top() + 15.0;
    let bottom = rect.bottom() - 14.0;
    p.text(Pos2::new(left, top), Align2::LEFT_CENTER, &e.name, egui::FontId::proportional(13.5), TEXT);
    p.text(
        Pos2::new(rect.right() - 8.0, top),
        Align2::RIGHT_CENTER,
        crate::osd::fmt_bytes(e.bytes),
        theme::mono(11.5),
        FAINT,
    );
    let mut sub = Vec::new();
    match &e.raw {
        Some(r) => {
            sub.push(if r.created_utc.is_empty() { e.modified.map(fmt_utc).unwrap_or_default() } else { fmt_created(&r.created_utc) });
            if let Some(d) = r.duration_s {
                sub.push(fmt_time(d).split('.').next().unwrap_or_default().to_string());
            }
            if let Some(m) = r.channel_mhz {
                sub.push(crate::channels::label(m));
            }
        }
        None => sub.push(e.modified.map(fmt_utc).unwrap_or_default()),
    }
    p.text(Pos2::new(left, bottom), Align2::LEFT_CENTER, sub.join("  \u{00B7}  "), egui::FontId::proportional(12.0), DIM);
    // Tags at the right of the second line.
    let mut x = rect.right() - 8.0;
    let mut tag = |text: &str, color: Color32| {
        let g = p.layout_no_wrap(text.to_string(), theme::mono(10.5), color);
        let tr = Rect::from_min_size(Pos2::new(x - g.size().x - 8.0, bottom - 8.0), Vec2::new(g.size().x + 8.0, 16.0));
        p.rect(tr, egui::CornerRadius::same(2), RAISED, Stroke::new(1.0, LINE), egui::StrokeKind::Inside);
        p.galley(tr.center() - g.size() / 2.0, g, color);
        x = tr.left() - 4.0;
    };
    if !e.videos.is_empty() {
        let t = if e.videos.len() > 1 { format!("VIDEO {}", e.videos.len()) } else { "VIDEO".into() };
        tag(&t, ACCENT);
    }
    if let Some(r) = &e.raw {
        if r.closed_cleanly == Some(false) {
            tag("OPEN", theme::WARN);
        }
        tag(if r.source == "sim" { "SIM" } else { "RAW" }, DIM);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The sidecar's facts as a two-column grid.
fn info_ui(ui: &mut egui::Ui, r: &RawInfo, bytes: u64) {
    theme::section(ui, "Raw recording");
    egui::Grid::new("raw-info").num_columns(2).spacing([16.0, 4.0]).striped(false).show(ui, |ui| {
        let mut row = |k: &str, v: String| {
            ui.label(RichText::new(k).color(DIM));
            ui.label(RichText::new(v).color(TEXT));
            ui.end_row();
        };
        if !r.created_utc.is_empty() {
            row("Created", fmt_created(&r.created_utc));
        }
        if let Some(d) = r.duration_s {
            row("Length", fmt_time(d));
        }
        if let Some(m) = r.channel_mhz {
            row("Channel", crate::channels::label(m));
        }
        row("Receivers", r.receivers.to_string());
        if !r.lanes.is_empty() {
            row("Lanes", r.lanes.clone());
        }
        if let Some(d) = r.divider {
            row("Clock", format!("/{d} ({:.2} MS/s)", crate::settings::divider_hz(d as u8) / 1e6));
        }
        if let Some(m) = &r.picture_mode {
            row("Picture mode", m.replace('_', " "));
        }
        row("Size", crate::osd::fmt_bytes(bytes));
        row(
            "Closed",
            match r.closed_cleanly {
                Some(true) => "cleanly".into(),
                Some(false) => "not cleanly (app stopped while recording)".into(),
                None => "--".into(),
            },
        );
        if !r.notes.is_empty() {
            row("Notes", r.notes.clone());
        }
    });
}

/// A thin seek bar. Returns (time, still dragging) when it was used.
fn scrubber(ui: &mut egui::Ui, w: f32, pos: f64, dur: f64) -> Option<(f64, bool)> {
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 20.0), Sense::click_and_drag());
    let p = ui.painter();
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 4.0));
    p.rect_filled(track, 2.0, RAISED);
    let f = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let done = Rect::from_min_max(track.min, Pos2::new(track.left() + track.width() * f, track.bottom()));
    p.rect_filled(done, 2.0, ACCENT);
    let knob = Pos2::new(done.right(), track.center().y);
    p.circle_filled(knob, if r.hovered() || r.dragged() { 7.0 } else { 5.0 }, TEXT);
    if dur <= 0.0 {
        return None;
    }
    let at = |x: f32| ((x - track.left()) / track.width()).clamp(0.0, 1.0) as f64 * dur;
    if r.dragged() {
        if let Some(pp) = r.interact_pointer_pos() {
            return Some((at(pp.x), true));
        }
    }
    if r.drag_stopped() || r.clicked() {
        if let Some(pp) = r.interact_pointer_pos() {
            return Some((at(pp.x), false));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(name: &str, bytes: u64, t: u64) -> Listed {
        Listed { name: name.into(), bytes, modified: Some(UNIX_EPOCH + std::time::Duration::from_secs(t)) }
    }

    #[test]
    fn grouping() {
        let files = [
            l("openvrx-1.openvrx.json", 1, 10),
            l("openvrx-1.rx0.bin", 100, 11),
            l("openvrx-1.events.jsonl", 2, 11),
            l("openvrx-1.mp4", 50, 12),
            l("openvrx-1-clean.mp4", 40, 30),
            l("openvrx-12.openvrx.json", 1, 20),
            l("openvrx-12.rx0.bin", 10, 20),
            l("openvrx-12-decoded.mp4", 5, 21),
            l("flight.webm", 7, 5),
            l("openvrx-10.mp4", 3, 4),
            l("notes.txt", 1, 40),
        ];
        let e = group(Path::new("/r"), &files);
        let names: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        // Newest first: openvrx-1 has the newest file (its clean video).
        // "openvrx-10.mp4" is not openvrx-1's: a lone video.
        assert_eq!(names, ["openvrx-1", "openvrx-12", "flight.webm", "openvrx-10.mp4"]);
        assert_eq!(e[0].bytes, 1 + 100 + 2 + 50 + 40);
        assert_eq!(e[0].videos.len(), 2);
        // "openvrx-12-decoded" is the longer base's, not openvrx-1's.
        assert_eq!(e[1].videos.len(), 1);
        assert_eq!(e[1].bytes, 16);
        assert!(e[2].raw.is_none());
    }

    #[test]
    fn sidecar_fields() {
        let j = r#"{"created_utc":"2026-10-29T21:14:03Z","source":"hardware","closed_cleanly":false,
            "receivers":[{"bytes":26666666,"sample_rate_hz":13333333.333,"channel_mhz":5880,
            "sample_format":{"preset":"6x6","divider":15,"lanes":{"word_bytes":2}}}],
            "context":{"app":{"picture_mode":"low_latency"}}}"#;
        let r = parse_sidecar(j, Path::new("/r/a.openvrx.json"));
        assert_eq!(r.channel_mhz, Some(5880));
        assert_eq!(r.lanes, "6x6");
        assert_eq!(r.divider, Some(15));
        assert_eq!(r.closed_cleanly, Some(false));
        assert_eq!(r.picture_mode.as_deref(), Some("low_latency"));
        assert!((r.duration_s.unwrap() - 1.0).abs() < 1e-3);
        let closed = r#"{"receivers":[],"context":{"summary":{"duration_s":12.5}}}"#;
        assert_eq!(parse_sidecar(closed, Path::new("x")).duration_s, Some(12.5));
        assert_eq!(parse_sidecar("not json", Path::new("x")).receivers, 0);
    }

    #[test]
    fn dates() {
        assert_eq!(fmt_utc(UNIX_EPOCH), "1970-01-01 00:00 UTC");
        assert_eq!(fmt_utc(UNIX_EPOCH + std::time::Duration::from_secs(1_793_304_000)), "2026-10-29 20:00 UTC");
        assert_eq!(fmt_created("2026-10-29T21:14:03Z"), "2026-10-29 21:14 UTC");
    }
}
