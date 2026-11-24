// SPDX-License-Identifier: GPL-3.0-or-later
//! The picture mode switch overlay: on a mode change (arrow keys or the menu)
//! a plate near the bottom of the window shows the previous, current and next
//! mode, the current mode's description and its place in the list. The cards
//! slide in from the side the step came from (clipped to the plate), the
//! plate holds, then fades. It is drawn on its own foreground layer and never
//! takes input, so it cannot get in the way of the picture.
//!
//! The persistent "which mode am I in" tag is the OSD badge at the top left
//! (`osd::paint_mode_badge`); this overlay is the moment of change.

use crate::settings::VideoPreset;
use crate::theme::{self, DIM, FAINT, LINE, TEXT};
use egui::{Align2, Color32, CornerRadius, FontId, Id, LayerId, Order, Pos2, Rect, Stroke, StrokeKind, Vec2};

/// Slide, total time on screen and fade-out, nanoseconds.
const SLIDE_NS: u64 = 140_000_000;
const SHOW_NS: u64 = 1_700_000_000;
const FADE_NS: u64 = 250_000_000;
/// Plate width, distance between neighbouring cards, card size.
const PLATE_W: f32 = 560.0;
const SPACING: f32 = 180.0;
const CARD: Vec2 = Vec2::new(164.0, 36.0);

#[derive(Default)]
pub struct PresetSwitch {
    /// When the last switch started, and its direction (+1 next, -1 previous).
    started_ns: Option<u64>,
    dir: f32,
}

impl PresetSwitch {
    /// A switch by `steps` positions (sign = direction; a menu pick can jump several).
    pub fn start(&mut self, steps: i32, now_ns: u64) {
        self.started_ns = Some(now_ns);
        self.dir = steps.signum() as f32;
    }

    /// Draws the plate for `current`; `note` is shown under the description
    /// (plugin mode with no plugin set). Asks for repaints while it is on screen.
    pub fn draw(&mut self, ctx: &egui::Context, current: VideoPreset, note: Option<&str>, now_ns: u64) {
        let Some(t0) = self.started_ns else { return };
        let t = now_ns.saturating_sub(t0);
        if t >= SHOW_NS {
            self.started_ns = None;
            return;
        }
        ctx.request_repaint();
        // Ease-out cubic slide; linear fade at the end.
        let s = (t as f32 / SLIDE_NS as f32).min(1.0);
        let slide = self.dir * SPACING * (1.0 - s).powi(3);
        let alpha = if t > SHOW_NS - FADE_NS { (SHOW_NS - t) as f32 / FADE_NS as f32 } else { 1.0 };

        let screen = ctx.content_rect();
        let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("openvrx-preset-switch")));
        let width = PLATE_W.min(screen.width() - 24.0);
        let pad = 14.0;
        // Wrapped description and note first: they set the plate's height.
        let wrap = width - 2.0 * pad;
        let desc = p.layout(current.description().to_string(), FontId::proportional(14.0), TEXT.gamma_multiply(alpha), wrap);
        let note = note.map(|n| p.layout(n.to_string(), FontId::proportional(12.5), theme::WARN.gamma_multiply(alpha), wrap));
        let head_h = 16.0;
        let height = pad + head_h + 8.0 + CARD.y + 12.0 + desc.size().y + note.as_ref().map_or(0.0, |g| g.size().y + 4.0) + pad;
        let plate = Rect::from_center_size(
            Pos2::new(screen.center().x, screen.bottom() - 48.0 - height / 2.0),
            Vec2::new(width, height),
        );
        p.rect(
            plate,
            CornerRadius::same(4),
            theme::SHEET.gamma_multiply(alpha),
            Stroke::new(1.0, LINE.gamma_multiply(alpha)),
            StrokeKind::Inside,
        );

        // Header: title left, position dots right.
        let head_y = plate.top() + pad + head_h / 2.0;
        p.text(
            Pos2::new(plate.left() + pad, head_y),
            Align2::LEFT_CENTER,
            "PICTURE MODE",
            theme::mono(11.0),
            FAINT.gamma_multiply(alpha),
        );
        let n = VideoPreset::ALL.len();
        let k0 = current.index();
        let mode_color = theme::mode_color(current == VideoPreset::Raw);
        for k in 0..n {
            let x = plate.right() - pad - (n - 1 - k) as f32 * 12.0 - 3.0;
            let c = if k == k0 { mode_color } else { LINE };
            p.circle_filled(Pos2::new(x, head_y), if k == k0 { 3.5 } else { 2.5 }, c.gamma_multiply(alpha));
        }
        p.text(
            Pos2::new(plate.right() - pad - n as f32 * 12.0 - 6.0, head_y),
            Align2::RIGHT_CENTER,
            "\u{2190} \u{2192}",
            theme::mono(11.0),
            FAINT.gamma_multiply(alpha),
        );

        // The strip, clipped to the plate so sliding cards never leave it.
        let strip_y = plate.top() + pad + head_h + 8.0 + CARD.y / 2.0;
        let clip = Rect::from_min_max(
            Pos2::new(plate.left() + 1.0, strip_y - CARD.y),
            Pos2::new(plate.right() - 1.0, strip_y + CARD.y),
        );
        let sp = p.with_clip_rect(clip);
        let ni = n as i32;
        for off in -2..=2i32 {
            let preset = VideoPreset::ALL[((k0 as i32 + off).rem_euclid(ni)) as usize];
            let x = plate.center().x + off as f32 * SPACING + slide;
            let dist = ((x - plate.center().x) / SPACING).abs();
            if dist > 1.6 {
                continue;
            }
            let near = (1.0 - dist).clamp(0.0, 1.0); // 1 at the centre, 0 one card away
            let rect = Rect::from_center_size(Pos2::new(x, strip_y), CARD);
            let pc = theme::mode_color(preset == VideoPreset::Raw);
            if near > 0.5 {
                sp.rect(
                    rect,
                    theme::R,
                    pc.gamma_multiply(0.16 * alpha * near),
                    Stroke::new(1.0, pc.gamma_multiply(alpha * near)),
                    StrokeKind::Inside,
                );
            }
            sp.text(
                rect.center(),
                Align2::CENTER_CENTER,
                preset.title(),
                FontId::proportional(14.0 + 3.0 * near),
                lerp(DIM, if near > 0.5 { TEXT } else { DIM }, near).gamma_multiply(alpha),
            );
        }

        // Description of the mode now running, then the note.
        let mut y = strip_y + CARD.y / 2.0 + 12.0;
        let dh = desc.size().y;
        p.galley(Pos2::new(plate.left() + pad, y), desc, TEXT);
        y += dh + 4.0;
        if let Some(g) = note {
            p.galley(Pos2::new(plate.left() + pad, y), g, theme::WARN);
        }
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
