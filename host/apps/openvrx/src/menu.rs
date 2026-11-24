// SPDX-License-Identifier: GPL-3.0-or-later
//! The Esc menu: a sheet docked on the left over the picture, with the live
//! state at the top, the band and channel picker, the picture modes with
//! their descriptions, and every shortcut as a row with its key.
//!
//! It opens on the frame the key is pressed: no fade-in (egui fades every
//! new area in by default) and the global animation time is short (see
//! `theme`). Up/Down move a cursor over the rows and Enter runs the row, so
//! the menu works without a mouse; the shortcut keys keep working while it
//! is open, and a click outside the sheet closes it.

use crate::channels::{self, BANDS};
use crate::keys::{key_name, Action, BINDINGS};
use crate::settings::VideoPreset;
use crate::theme::{self, ACCENT, ACCENT_SOFT, DIM, FAINT, HOVER, LINE, RAISED, TEXT};
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::sync::mpsc;

/// Sheet width (points; the UI scale applies on top).
const WIDTH: f32 = 400.0;
const ROW_H: f32 = 30.0;

/// Row groups in display order. Actions not listed (Escape, the arrow keys,
/// the channel keys, which have their own pickers) stay key-only.
const GROUPS: &[(&str, &[Action])] = &[
    ("View", &[Action::Fullscreen, Action::Layout, Action::Hd3, Action::Aspect, Action::Scaling, Action::Osd]),
    ("Picture", &[Action::SteadyOsd, Action::Damage, Action::Compare]),
    ("Recording", &[Action::Record, Action::RecordVideo, Action::Library]),
    ("Receiver", &[Action::Calibrate, Action::ResetStats]),
    ("Presentation", &[Action::PresentMode, Action::PresentTrigger]),
    ("App", &[Action::Panels, Action::Help, Action::Quit]),
];

/// Short menu label (the F1 help keeps the long text).
fn label(a: Action) -> &'static str {
    match a {
        Action::Help => "All keys",
        Action::Fullscreen => "Fullscreen",
        Action::Panels => "Settings panel",
        Action::Osd => "OSD",
        Action::Record => "Record raw",
        Action::RecordVideo => "Record video (MP4)",
        Action::Library => "Recordings",
        Action::Preset => "Next picture mode",
        Action::PresetPrev => "Previous picture mode",
        Action::SteadyOsd => "Steady OSD",
        Action::Hd3 => "HD3 goggle mode",
        Action::Layout => "Views",
        Action::PresentMode => "Present mode",
        Action::PresentTrigger => "Present trigger",
        Action::Aspect => "Aspect",
        Action::Scaling => "Scaling",
        Action::Damage => "Damage overlay",
        Action::Calibrate => "Calibrate phase",
        Action::Compare => "Compare with test card",
        Action::ChannelDown => "Channel down",
        Action::ChannelUp => "Channel up",
        Action::ResetStats => "Reset latency stats",
        Action::Escape => "Close",
        Action::Quit => "Quit",
    }
}

/// Actions that close the menu (they open something else or end the app).
fn closes_menu(a: Action) -> bool {
    matches!(a, Action::Help | Action::Panels | Action::Quit | Action::Compare | Action::Library)
}

/// What the app shows next to a row: an on/off state, or a current value.
pub enum RowState {
    None,
    On(bool),
    Value(String),
}

/// What the user picked this frame.
pub enum MenuPick {
    Act(Action),
    Tune(u32),
    Preset(VideoPreset),
}

/// The live state shown in the menu's header.
pub struct MenuStatus {
    pub mhz: u32,
    pub preset: VideoPreset,
    /// Why the preset cannot run as chosen (plugin mode with no plugin).
    pub preset_note: Option<&'static str>,
    /// View 1's lock state, colour and C/N of its best receiver.
    pub lock: (&'static str, Color32),
    pub cn_db: Option<f32>,
    /// "REC 1:02", "VID 0:40" while recording.
    pub recording: Vec<String>,
}

pub struct Menu {
    pub open: bool,
    /// Band whose channels are shown (follows the current channel on open).
    band: char,
    /// A tune running on the C5 console (FX2-only mode): its result.
    tuning: Option<(u32, mpsc::Receiver<Result<u32, String>>)>,
    /// Keyboard cursor over the action rows (None until Up/Down is pressed).
    cursor: Option<usize>,
    /// The cursor moved this frame: bring its row into view.
    scroll_to_cursor: bool,
    /// The frame the menu opened on (its own click must not close it).
    opened_frame: u64,
}

impl Menu {
    pub fn new() -> Menu {
        Menu {
            open: false,
            band: 'R',
            tuning: None,
            cursor: None,
            scroll_to_cursor: false,
            opened_frame: 0,
        }
    }

    pub fn toggle(&mut self, mhz: u32, frame: u64) {
        self.open = !self.open;
        if self.open {
            self.cursor = None;
            self.opened_frame = frame;
            if let Some((b, _)) = channels::name_for(mhz) {
                self.band = b;
            }
        }
    }

    /// Starts a tune on the C5 console in the background.
    pub fn start_tune(&mut self, mhz: u32) {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("openvrx-menu-tune".into())
            .spawn(move || {
                let _ = tx.send(tune_on_console(mhz));
            })
            .ok();
        self.tuning = Some((mhz, rx));
    }

    /// Result of a finished background tune.
    pub fn poll_tune(&mut self) -> Option<Result<u32, String>> {
        let (_, rx) = self.tuning.as_ref()?;
        match rx.try_recv() {
            Ok(r) => {
                self.tuning = None;
                Some(r)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.tuning = None;
                Some(Err("tune thread ended".into()))
            }
        }
    }

    /// Draws the menu; returns what was picked.
    pub fn show(&mut self, ctx: &egui::Context, st: &MenuStatus, state: impl Fn(Action) -> RowState) -> Vec<MenuPick> {
        let mut picks = Vec::new();
        if !self.open {
            return picks;
        }
        let rows: Vec<Action> = GROUPS.iter().flat_map(|g| g.1.iter().copied()).collect();
        self.keyboard(ctx, &rows, &mut picks);

        let screen = ctx.content_rect();
        let frame = egui::Frame::NONE
            .fill(theme::SHEET)
            .stroke(Stroke::new(1.0, LINE))
            .inner_margin(egui::Margin { left: 18, right: 18, top: 16, bottom: 12 });
        let resp = egui::Area::new(egui::Id::unique("openvrx-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .fade_in(false)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(WIDTH);
                    ui.set_min_height(screen.height() - 28.0);
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                    self.header(ui, st);
                    divider(ui);
                    let footer_h = 30.0;
                    egui::ScrollArea::vertical()
                        .max_height((ui.available_height() - footer_h).max(120.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            self.channel_picker(ui, st.mhz, &mut picks);
                            divider(ui);
                            preset_picker(ui, st.preset, st.preset_note, &mut picks);
                            divider(ui);
                            self.actions(ui, &state, &mut picks);
                        });
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| footer(ui));
                });
            });
        // A click outside the sheet closes it (not the click that opened it).
        let clicked_outside = ctx.input(|i| i.pointer.any_click())
            && !resp.response.contains_pointer()
            && ctx.cumulative_frame_nr() > self.opened_frame;
        if clicked_outside {
            self.open = false;
        }
        for p in &picks {
            if let MenuPick::Act(a) = p {
                if closes_menu(*a) {
                    self.open = false;
                }
            }
        }
        picks
    }

    /// Up/Down move the cursor (wrapping), Enter runs the row under it.
    fn keyboard(&mut self, ctx: &egui::Context, rows: &[Action], picks: &mut Vec<MenuPick>) {
        if ctx.egui_wants_keyboard_input() || rows.is_empty() {
            return;
        }
        let n = rows.len();
        let (up, down, enter) = ctx.input(|i| {
            (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Enter))
        });
        self.scroll_to_cursor = up || down;
        if down {
            self.cursor = Some(self.cursor.map_or(0, |c| (c + 1) % n));
        }
        if up {
            self.cursor = Some(self.cursor.map_or(n - 1, |c| (c + n - 1) % n));
        }
        if enter {
            if let Some(c) = self.cursor {
                picks.push(MenuPick::Act(rows[c]));
            }
        }
    }

    fn header(&self, ui: &mut egui::Ui, st: &MenuStatus) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("OpenVRX").size(theme::SIZE_TITLE).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, color) = match &self.tuning {
                    Some((m, _)) => (format!("tuning {}", channels::label(*m)), DIM),
                    None => (channels::label(st.mhz), TEXT),
                };
                ui.label(RichText::new(text).size(16.0).color(color));
            });
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let mode_color = theme::mode_color(st.preset == VideoPreset::Raw);
            badge(ui, st.preset.title(), mode_color);
            badge(ui, st.lock.0, st.lock.1);
            if let Some(cn) = st.cn_db {
                ui.label(RichText::new(format!("C/N {cn:.1}")).font(theme::mono(12.5)).color(DIM));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for r in st.recording.iter().rev() {
                    ui.label(RichText::new(r).font(theme::mono(12.5)).color(theme::BAD));
                }
            });
        });
    }

    fn channel_picker(&mut self, ui: &mut egui::Ui, mhz: u32, picks: &mut Vec<MenuPick>) {
        ui.horizontal(|ui| {
            theme::section(ui, "Channel");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("[  ]").font(theme::mono(11.0)).color(FAINT));
            });
        });
        let current = channels::name_for(mhz);
        // Band tabs as one segmented row; a dot marks the band on air.
        let w = ui.available_width();
        let bw = (w - 4.0 * (BANDS.len() as f32 - 1.0)) / BANDS.len() as f32;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for b in BANDS.iter() {
                let on = b.letter == self.band;
                let live = current.is_some_and(|c| c.0 == b.letter);
                let (rect, r) = ui.allocate_exact_size(Vec2::new(bw, 26.0), Sense::click());
                let p = ui.painter();
                let fill = if on { RAISED } else if r.hovered() { HOVER } else { Color32::TRANSPARENT };
                p.rect(rect, theme::R, fill, Stroke::new(1.0, if on { LINE } else { Color32::TRANSPARENT }), egui::StrokeKind::Inside);
                p.text(rect.center(), Align2::CENTER_CENTER, b.letter, theme::body(), if on { TEXT } else { DIM });
                if live {
                    p.circle_filled(Pos2::new(rect.right() - 7.0, rect.top() + 7.0), 2.5, ACCENT);
                }
                if r.on_hover_text(b.name).clicked() {
                    self.band = b.letter;
                }
            }
        });
        ui.add_space(2.0);
        let band = channels::band(self.band).unwrap_or(&BANDS[0]);
        let busy = self.tuning.is_some();
        let cw = (w - 4.0 * 7.0) / 8.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (k, &f) in band.mhz.iter().enumerate() {
                let on = f == mhz;
                let sense = if busy { Sense::hover() } else { Sense::click() };
                let (rect, r) = ui.allocate_exact_size(Vec2::new(cw, 40.0), sense);
                let p = ui.painter();
                let fill = if on { ACCENT } else if r.hovered() && !busy { HOVER } else { RAISED };
                p.rect_filled(rect, theme::R, fill);
                let (c1, c2) = if on { (theme::ON_ACCENT, theme::ON_ACCENT) } else { (TEXT, FAINT) };
                p.text(rect.center_top() + Vec2::new(0.0, 12.0), Align2::CENTER_CENTER, (k + 1).to_string(), theme::body(), c1);
                p.text(rect.center_bottom() - Vec2::new(0.0, 11.0), Align2::CENTER_CENTER, f.to_string(), theme::mono(10.5), c2);
                if r.clicked() && !on {
                    picks.push(MenuPick::Tune(f));
                }
            }
        });
    }

    fn actions(&self, ui: &mut egui::Ui, state: &impl Fn(Action) -> RowState, picks: &mut Vec<MenuPick>) {
        let mut idx = 0;
        for (g, (title, list)) in GROUPS.iter().enumerate() {
            if g > 0 {
                ui.add_space(6.0);
            }
            theme::section(ui, title);
            for &a in list.iter() {
                let focused = self.cursor == Some(idx);
                idx += 1;
                let Some((key, ctrl, _, help)) = BINDINGS.iter().find(|b| b.2 == a) else {
                    continue;
                };
                let r = row(ui, label(a), &key_name(*key, *ctrl), state(a), focused);
                if focused && self.scroll_to_cursor {
                    r.scroll_to_me(None);
                }
                if r.on_hover_text(*help).clicked() {
                    picks.push(MenuPick::Act(a));
                }
            }
        }
    }
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, LINE);
    ui.add_space(8.0);
}

/// A small filled tag ("LOW LATENCY", "LOCKED").
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), theme::mono(11.5), color);
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(2), color.gamma_multiply(0.14), Stroke::new(1.0, color.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// Key hints along the bottom of the sheet.
fn footer(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter();
    let y = rect.center().y;
    let mut x = rect.left();
    for (key, text) in [("Esc", "close"), ("\u{2191}\u{2193}", "move"), ("Enter", "run"), ("\u{2190}\u{2192}", "mode")] {
        let cap = theme::key_cap(p, Pos2::new(x, y), Align2::LEFT_CENTER, key);
        let t = p.text(
            Pos2::new(cap.right() + 5.0, y),
            Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(theme::SIZE_SMALL),
            FAINT,
        );
        x = t.right() + 14.0;
    }
}

/// Tunes the C5 over its console and saves it (FX2-only mode, where the app
/// has no C5 link of its own). Pushes no other setting.
fn tune_on_console(mhz: u32) -> Result<u32, String> {
    use openvrx_hw::c5::{find_c5_ports, C5Client, ClientConfig, PortConnector, PortSelector};
    let port = find_c5_ports()
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .map(|p| p.port_name)
        .ok_or("no C5 on USB: plug its USB in to change channel")?;
    let c5 = C5Client::start(PortConnector::new(PortSelector::Name(port.clone())), None, ClientConfig::default())
        .map_err(|e| e.to_string())?;
    if !c5.wait_connected(std::time::Duration::from_secs(3)) {
        return Err(format!("the C5 on {port} does not answer"));
    }
    let (got, _) = c5.tune(mhz, 0).map_err(|e| format!("tune: {e}"))?;
    c5.save().map_err(|e| format!("save: {e}"))?;
    Ok(got)
}

/// The picture modes as a list in arrow-key order, and a fixed box below
/// with the description of the hovered (or current) one, so the layout does
/// not jump while the pointer moves.
fn preset_picker(ui: &mut egui::Ui, current: VideoPreset, note: Option<&str>, picks: &mut Vec<MenuPick>) {
    ui.horizontal(|ui| {
        theme::section(ui, "Picture mode");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("\u{2190} \u{2192}").font(theme::mono(11.0)).color(FAINT));
        });
    });
    let mut shown = current;
    let w = ui.available_width();
    for p in VideoPreset::ALL {
        let on = p == current;
        let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
        let painter = ui.painter();
        if on {
            painter.rect_filled(rect, theme::R, ACCENT_SOFT);
            painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), 0.0, theme::mode_color(p == VideoPreset::Raw));
        } else if r.hovered() {
            painter.rect_filled(rect, theme::R, HOVER);
        }
        painter.text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, p.title(), theme::body(), if on { TEXT } else { DIM });
        if on {
            painter.text(rect.right_center() - Vec2::new(10.0, 0.0), Align2::RIGHT_CENTER, "ACTIVE", theme::mono(10.5), ACCENT);
        }
        if r.hovered() {
            shown = p;
        }
        if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
            picks.push(MenuPick::Preset(p));
        }
    }
    ui.add_space(4.0);
    egui::Frame::NONE
        .fill(theme::BG)
        .corner_radius(theme::R)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(38.0);
            ui.label(RichText::new(shown.title()).size(theme::SIZE_SMALL).color(if shown == current { ACCENT } else { DIM }));
            ui.label(RichText::new(shown.description()).size(13.0).color(TEXT));
            if shown == current {
                if let Some(n) = note {
                    ui.label(RichText::new(n).size(theme::SIZE_SMALL).color(theme::WARN));
                }
            }
        });
}

/// One clickable row: label left, state and key cap right.
fn row(ui: &mut egui::Ui, text: &str, key: &str, state: RowState, focused: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    let p = ui.painter();
    if focused {
        p.rect_filled(rect, theme::R, ACCENT_SOFT);
        p.rect_filled(Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), 0.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, theme::R, HOVER);
    }
    let mid = rect.center().y;
    p.text(Pos2::new(rect.left() + 12.0, mid), Align2::LEFT_CENTER, text, theme::body(), TEXT);
    let cap = theme::key_cap(p, Pos2::new(rect.right() - 8.0, mid), Align2::RIGHT_CENTER, key);
    let (s, color) = match state {
        RowState::None => (String::new(), DIM),
        RowState::On(true) => ("ON".to_string(), ACCENT),
        RowState::On(false) => ("OFF".to_string(), FAINT),
        RowState::Value(v) => (v, DIM),
    };
    if !s.is_empty() {
        let font = if matches!(s.as_str(), "ON" | "OFF") { theme::mono(11.5) } else { egui::FontId::proportional(13.0) };
        p.text(Pos2::new(cap.left() - 10.0, mid), Align2::RIGHT_CENTER, s, font, color);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
