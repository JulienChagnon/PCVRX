// SPDX-License-Identifier: GPL-3.0-or-later
//! Control panels (Tab). They edit `app.settings`; the app turns changes
//! into backend commands, swap-chain and window updates.

use crate::app::OpenvrxApp;
use crate::channels::{self, BANDS};
use crate::commands::AppCommand;
use crate::osd::{fmt_bytes, fmt_cn, fmt_elapsed};
use crate::settings::{
    AspectMode, BoardKind, DiversityMode, GainMode, OutputAspect, PresentModeSetting,
    PresentTrigger, ScalingFilter, StandardChoice, VideoPreset, ViewLayout, DEFAULT_GAIN,
    DIVIDERS, PLUGIN_DEADLINE_MS,
};
use crate::theme;
use egui::{CollapsingHeader, ComboBox, DragValue, RichText, Slider, Ui};
use openvrx_core::clock::now_ns;
use openvrx_core::LanePreset;

fn combo<T: Copy + PartialEq>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    all: &[T],
    name: impl Fn(T) -> String,
) {
    ComboBox::from_id_salt(id)
        .selected_text(name(*value))
        .show_ui(ui, |ui| {
            for &v in all {
                ui.selectable_value(value, v, name(v));
            }
        });
}

/// A section: a clickable title in the theme's small caps, then its body.
fn section(ui: &mut Ui, title: &str, open: bool, body: impl FnOnce(&mut Ui)) {
    ui.add_space(4.0);
    CollapsingHeader::new(RichText::new(title.to_uppercase()).size(theme::SIZE_LABEL).color(theme::DIM).extra_letter_spacing(0.8))
        .default_open(open)
        .show_unindented(ui, |ui| {
            ui.add_space(2.0);
            body(ui);
        });
    ui.add_space(4.0);
    ui.separator();
}

pub fn show(ui: &mut Ui, app: &mut OpenvrxApp) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Settings").size(theme::SIZE_TITLE).color(theme::TEXT));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("Tab").font(theme::mono(11.5)).color(theme::FAINT));
        });
    });
    ui.add_space(6.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        section(ui, "Receivers", true, |ui| receivers(ui, app));
        section(ui, "Video", true, |ui| video(ui, app));
        section(ui, "Recording", true, |ui| recording(ui, app));
        section(ui, "Display", false, |ui| display(ui, app));
        section(ui, "Status", false, |ui| status(ui, app));
    });
}

/// Label column of the settings grids.
fn key(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::DIM));
}

fn receivers(ui: &mut Ui, app: &mut OpenvrxApp) {
    let mut calibrate = None;
    let n = app.settings.receivers.len();
    for k in 0..n {
        let live = app
            .view_status(0)
            .receivers
            .iter()
            .find(|r| r.rx as usize == k)
            .cloned();
        let r = &mut app.settings.receivers[k];
        let card = egui::Frame::NONE
            .fill(theme::BG)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(theme::R)
            .inner_margin(egui::Margin::same(10));
        card.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.checkbox(&mut r.enabled, "");
                ui.label(RichText::new(format!("rx{k}")).font(theme::mono(13.5)).color(theme::TEXT));
                if let Some(s) = &live {
                    let (lock, color) = if s.sync_locked { ("LOCKED", theme::GOOD) } else { ("NO SYNC", theme::BAD) };
                    ui.label(RichText::new(lock).font(theme::mono(11.5)).color(color));
                    ui.label(
                        RichText::new(format!("C/N {}  gain {}", fmt_cn(s.cn_db), s.gain))
                            .font(theme::mono(12.0))
                            .color(theme::DIM),
                    );
                }
            });
            if r.control == crate::settings::ControlMode::Fx2Only {
                ui.label(RichText::new(format!(
                    "FX2-only mode: channel and gain are the C5's saved ones{} and the controls below do nothing. Lanes and clock must match the C5.",
                    r.fx2_only_mhz.map(|m| format!(" ({})", channels::label(m))).unwrap_or_default()
                )).size(theme::SIZE_SMALL).color(theme::WARN));
            }
            ui.horizontal(|ui| {
                ui.label("Channel");
                let current = channels::name_for(r.freq_mhz);
                for b in &BANDS {
                    let sel = current.is_some_and(|c| c.0 == b.letter);
                    let mut menu = ComboBox::from_id_salt(format!("band{k}{}", b.letter))
                        .width(36.0)
                        .selected_text(match current {
                            Some((l, c)) if sel => format!("{l}{c}"),
                            _ => b.letter.to_string(),
                        });
                    menu = menu.height(240.0);
                    menu.show_ui(ui, |ui| {
                        for (i, &f) in b.mhz.iter().enumerate() {
                            ui.selectable_value(&mut r.freq_mhz, f, format!("{}{} {f}", b.letter, i + 1));
                        }
                    });
                }
            });
            ui.horizontal(|ui| {
                ui.label("MHz");
                ui.add(DragValue::new(&mut r.freq_mhz).range(channels::MIN_MHZ..=channels::MAX_MHZ));
                ui.label(channels::label(r.freq_mhz));
            });
            ui.horizontal(|ui| {
                ui.label("Gain");
                let mut auto = r.gain == GainMode::Auto;
                if ui.checkbox(&mut auto, "auto").changed() {
                    r.gain = if auto {
                        GainMode::Auto
                    } else {
                        GainMode::Manual {
                            index: live.as_ref().map_or(DEFAULT_GAIN, |s| s.gain),
                        }
                    };
                }
                if let GainMode::Manual { index } = &mut r.gain {
                    ui.add(Slider::new(index, 0..=77));
                }
            });
            ui.horizontal(|ui| {
                ui.label("Board");
                combo(ui, &format!("board{k}"), &mut r.board, &[BoardKind::Xiao, BoardKind::Xiao7x7, BoardKind::Zero, BoardKind::Custom], |b| {
                    b.name().to_string()
                });
                ui.label("Lanes");
                combo(ui, &format!("lanes{k}"), &mut r.lanes, &LanePreset::ALL, |p| p.name().to_string());
            });
            ui.horizontal(|ui| {
                ui.label("Divider");
                combo(ui, &format!("div{k}"), &mut r.divider, &DIVIDERS, |d| {
                    format!("/{d} ({:.2} MS/s)", crate::settings::divider_hz(d) / 1e6)
                });
                if ui.button("Calibrate").clicked() {
                    calibrate = Some(k as u8);
                }
            });
            ui.horizontal(|ui| {
                ui.label("C5 port");
                let mut port = r.c5_port.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0).hint_text("auto")).changed() {
                    r.c5_port = (!port.trim().is_empty()).then(|| port.trim().to_string());
                }
                ui.label("FX2");
                let mut serial = r.fx2_serial.clone().unwrap_or_default();
                if ui.add(egui::TextEdit::singleline(&mut serial).desired_width(80.0).hint_text("auto")).changed() {
                    r.fx2_serial = (!serial.trim().is_empty()).then(|| serial.trim().to_string());
                }
            });
        });
    }
    ui.horizontal(|ui| {
        if ui.button("Add receiver").clicked() && n < crate::settings::MAX_RECEIVERS {
            let mut r = app.settings.receivers.last().cloned().unwrap_or_default();
            r.name = format!("rx{n}");
            r.c5_port = None;
            r.fx2_serial = None;
            app.settings.receivers.push(r);
        }
        if ui.button("Remove last").clicked() && n > 1 {
            app.settings.receivers.pop();
        }
    });
    ui.label("Port and serial changes apply on the next start.");
    if let Some(rx) = calibrate {
        app.send(AppCommand::Calibrate { rx });
    }
}

fn video(ui: &mut Ui, app: &mut OpenvrxApp) {
    let preset = app.settings.video.preset;
    let mut picked = preset;
    egui::Grid::new("video-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        let v = &mut app.settings.video;
        key(ui, "Picture mode");
        combo(ui, "preset", &mut picked, &VideoPreset::ALL, |p| p.title().to_string());
        ui.end_row();
        key(ui, "Standard");
        combo(ui, "standard", &mut v.standard, &StandardChoice::ALL, |s| s.name().to_string());
        ui.end_row();
        key(ui, "Diversity");
        combo(ui, "diversity", &mut v.diversity, &DiversityMode::ALL, |d| d.name().to_string());
        ui.end_row();
    });
    ui.label(RichText::new(picked.description()).size(theme::SIZE_SMALL).color(theme::DIM));
    if picked != preset {
        // Through the app, so the switch overlay shows like for the keys.
        app.set_preset(picked, picked.index() as i32 - preset.index() as i32);
    }
    let v = &mut app.settings.video;
    ui.checkbox(&mut v.steady_osd, "Steady OSD (fields line-doubled like goggles)");
    ui.add_space(6.0);
    theme::section(ui, "Filter plugin (plugin mode)");
    egui::Grid::new("plugin-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        key(ui, "Library");
        let mut s = v.plugin_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        if ui.add(egui::TextEdit::singleline(&mut s).hint_text("path to .so / .dll").desired_width(f32::INFINITY)).changed() {
            v.plugin_path = (!s.trim().is_empty()).then(|| s.trim().into());
        }
        ui.end_row();
        key(ui, "Config");
        let mut s = v.plugin_config_text();
        if ui.add(egui::TextEdit::singleline(&mut s).desired_width(f32::INFINITY)).changed() {
            v.plugin_config = serde_json::Value::String(s);
        }
        ui.end_row();
        key(ui, "Deadline");
        ui.add(Slider::new(&mut v.plugin_deadline_ms, PLUGIN_DEADLINE_MS.0..=PLUGIN_DEADLINE_MS.1).suffix(" ms"));
        ui.end_row();
    });
    ui.label(
        RichText::new("Library or config changes restart the plugin; the plain pipeline shows meanwhile.")
            .size(theme::SIZE_SMALL)
            .color(theme::FAINT),
    );
}

fn recording(ui: &mut Ui, app: &mut OpenvrxApp) {
    let rec = app.backend_status().recording.clone();
    ui.horizontal(|ui| {
        ui.label("Folder");
        let mut s = app.settings.recording.folder.display().to_string();
        if ui.text_edit_singleline(&mut s).changed() {
            app.settings.recording.folder = s.into();
        }
    });
    if rec.active {
        ui.label(
            RichText::new(format!(
                "REC {}  {}",
                fmt_elapsed(now_ns().saturating_sub(rec.started_ns)),
                fmt_bytes(rec.bytes)
            ))
            .font(theme::mono(12.5))
            .color(theme::BAD),
        );
        if let Some(p) = &rec.path {
            ui.label(RichText::new(p.display().to_string()).size(theme::SIZE_SMALL).color(theme::DIM));
        }
    }
    ui.horizontal(|ui| {
        let label = if rec.active { "Stop raw (R)" } else { "Record raw (R)" };
        if ui.add_enabled(rec.available || rec.active, theme::toggle_button(label, rec.active)).clicked() {
            app.toggle_recording();
        }
        let video = rec.video.active;
        let label = if video { "Stop video (M)" } else { "Record video (M)" };
        if ui.add_enabled(rec.available || video, theme::toggle_button(label, video)).clicked() {
            app.toggle_video_recording();
        }
        if ui.button("Recordings (B)").clicked() {
            app.open_library();
        }
    });
}

fn display(ui: &mut Ui, app: &mut OpenvrxApp) {
    let d = &mut app.settings.display;
    egui::Grid::new("display-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        key(ui, "Views");
        combo(ui, "layout", &mut d.layout, &ViewLayout::ALL, |l| l.name().to_string());
        ui.end_row();
        key(ui, "Aspect");
        ui.horizontal(|ui| {
            combo(ui, "aspect", &mut d.aspect, &AspectMode::ALL, |a| a.name().to_string());
            combo(ui, "outaspect", &mut d.output_aspect, &OutputAspect::ALL, |a| a.name().to_string());
        });
        ui.end_row();
        key(ui, "Scaling");
        combo(ui, "scaling", &mut d.scaling, &ScalingFilter::ALL, |s| s.name().to_string());
        ui.end_row();
        key(ui, "Overscan");
        ui.add(Slider::new(&mut d.overscan_percent, 0.0..=15.0).suffix(" %"));
        ui.end_row();
        key(ui, "Sharpen");
        ui.add(Slider::new(&mut d.sharpen, 0.0..=2.0));
        ui.end_row();
        key(ui, "Brightness");
        ui.add(Slider::new(&mut d.brightness, -0.5..=0.5));
        ui.end_row();
        key(ui, "Contrast");
        ui.add(Slider::new(&mut d.contrast, 0.25..=3.0));
        ui.end_row();
        key(ui, "Saturation");
        ui.add(Slider::new(&mut d.saturation, 0.0..=3.0));
        ui.end_row();
        key(ui, "UI scale");
        ui.add(Slider::new(&mut d.ui_scale, 0.5..=3.0));
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.checkbox(&mut d.fullscreen, "Fullscreen (F11)");
    ui.checkbox(&mut d.hd3_mode, "HD3 mode (H)");
    ui.checkbox(&mut d.hd3_stretch, "HD3: stretch to full width");
    ui.checkbox(&mut d.osd, "OSD (O)");
    ui.checkbox(&mut d.row_strip, "Receiver strip");
    ui.checkbox(&mut d.damage_overlay, "Damage overlay (D)");
    ui.add_space(6.0);
    theme::section(ui, "Presentation");
    egui::Grid::new("present-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        key(ui, "Present");
        ui.horizontal(|ui| {
            combo(ui, "present", &mut d.present_mode, &PresentModeSetting::ALL, |m| m.name().to_string());
            combo(ui, "trigger", &mut d.present_trigger, &PresentTrigger::ALL, |t| t.name().to_string());
        });
        ui.end_row();
        if d.present_trigger == PresentTrigger::Lines {
            key(ui, "Lines per slice");
            ui.add(Slider::new(&mut d.present_lines, 4..=288));
            ui.end_row();
        }
        key(ui, "Max presents/s");
        ui.add(Slider::new(&mut d.max_present_hz, 0..=2000)).on_hover_text("0 = no cap");
        ui.end_row();
        key(ui, "Frame latency");
        ui.add(Slider::new(&mut d.max_frame_latency, 1..=3));
        ui.end_row();
    });
}

fn status(ui: &mut Ui, app: &mut OpenvrxApp) {
    let s = app.backend_status().clone();
    let lat = app.latency_summary();
    egui::Grid::new("status-grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        key(ui, "Backend");
        ui.label(&s.name);
        ui.end_row();
        key(ui, "GPU");
        ui.label(app.adapter_name());
        ui.end_row();
        key(ui, "Latency");
        ui.label(RichText::new(crate::osd::fmt_latency(&lat)).font(theme::mono(12.5)));
        ui.end_row();
    });
    for d in &s.devices {
        ui.label(RichText::new(d).size(theme::SIZE_SMALL).color(theme::DIM));
    }
    for w in &s.warnings {
        ui.label(RichText::new(w).color(theme::WARN));
    }
}
