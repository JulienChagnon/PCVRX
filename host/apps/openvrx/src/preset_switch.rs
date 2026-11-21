// SPDX-License-Identifier: GPL-3.0-or-later
//! The preset switch overlay: on a preset change (arrow keys or the menu) a
//! strip with the previous, current and next preset slides in from the side
//! the step came from, holds, then fades. It is drawn on its own foreground
//! layer and never takes input, so it cannot get in the way of the picture.

use crate::settings::VideoPreset;
use egui::{Align2, Color32, CornerRadius, FontId, Id, LayerId, Order, Pos2, Rect, Stroke, Vec2};

/// Slide, total time on screen and fade-out, nanoseconds.
const SLIDE_NS: u64 = 220_000_000;
const SHOW_NS: u64 = 1_600_000_000;
const FADE_NS: u64 = 400_000_000;
/// Distance between neighbouring cards and their size.
const SPACING: f32 = 190.0;
const CARD: Vec2 = Vec2::new(170.0, 44.0);

const BG: Color32 = Color32::from_rgba_premultiplied(16, 18, 22, 225);
const ACCENT: Color32 = Color32::from_rgb(64, 182, 166);
const TEXT: Color32 = Color32::from_rgb(222, 226, 232);
const DIM: Color32 = Color32::from_rgb(132, 140, 152);

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

    /// Draws the strip for `current`; `note` is shown under the description
    /// (cleanup off, no plugin). Asks for repaints while it is on screen.
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
        let centre = Pos2::new(screen.center().x, screen.bottom() - 120.0);
        let p = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("openvrx-preset-switch")));
        let n = VideoPreset::ALL.len() as i32;
        let k0 = current.index() as i32;
        // Two cards each side, so the one sliding in is there before it is on screen.
        for off in -2..=2i32 {
            let preset = VideoPreset::ALL[((k0 + off).rem_euclid(n)) as usize];
            let x = centre.x + off as f32 * SPACING + slide;
            let dist = ((x - centre.x) / SPACING).abs();
            if dist > 1.6 {
                continue;
            }
            let near = (1.0 - dist).clamp(0.0, 1.0);   // 1 at the centre, 0 one card away
            let fade = alpha * (1.0 - (dist - 1.0).max(0.0) / 0.6);
            let scale = 0.82 + 0.18 * near;
            let rect = Rect::from_center_size(Pos2::new(x, centre.y), CARD * scale);
            p.rect(
                rect,
                CornerRadius::same(10),
                BG.gamma_multiply(fade),
                Stroke::new(1.5, lerp_color(DIM, ACCENT, near).gamma_multiply(fade * (0.3 + 0.7 * near))),
                egui::StrokeKind::Inside,
            );
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                preset.title(),
                FontId::proportional(13.0 + 5.0 * near),
                lerp_color(DIM, TEXT, near).gamma_multiply(fade),
            );
        }
        // Description of the preset now running, under the strip.
        let mut y = centre.y + CARD.y / 2.0 + 16.0;
        p.text(
            Pos2::new(centre.x, y),
            Align2::CENTER_TOP,
            current.description(),
            FontId::proportional(13.0),
            TEXT.gamma_multiply(alpha),
        );
        if let Some(note) = note {
            y += 20.0;
            p.text(
                Pos2::new(centre.x, y),
                Align2::CENTER_TOP,
                note,
                FontId::proportional(12.0),
                Color32::from_rgb(230, 170, 80).gamma_multiply(alpha),
            );
        }
        p.text(
            Pos2::new(centre.x, centre.y - CARD.y / 2.0 - 10.0),
            Align2::CENTER_BOTTOM,
            "\u{2190}  preset  \u{2192}",
            FontId::proportional(11.0),
            DIM.gamma_multiply(alpha),
        );
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
