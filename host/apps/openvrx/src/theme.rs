// SPDX-License-Identifier: GPL-3.0-or-later
//! One look for every part of the app: colours, sizes, the egui style, the
//! UI font and a few painter helpers shared by the menu, the overlays, the
//! OSD and the recordings page.
//!
//! The rules: flat dark surfaces, 1 px borders, small corner radii, one
//! accent colour for "current / on", and animations short enough that
//! nothing waits for them (egui's default is 0.2 s, which made the menu feel
//! slow; here it is 0.05 s and the overlays turn their fade-in off).

use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

/// Window and page background.
pub const BG: Color32 = Color32::from_rgb(11, 12, 15);
/// Panels and sheets.
pub const SURFACE: Color32 = Color32::from_rgb(19, 21, 26);
/// Raised items on a surface: chips, inputs, idle buttons.
pub const RAISED: Color32 = Color32::from_rgb(30, 33, 40);
/// Hovered item.
pub const HOVER: Color32 = Color32::from_rgb(40, 44, 53);
/// Borders and separators.
pub const LINE: Color32 = Color32::from_rgb(46, 50, 60);
pub const TEXT: Color32 = Color32::from_rgb(232, 235, 240);
pub const DIM: Color32 = Color32::from_rgb(156, 163, 176);
pub const FAINT: Color32 = Color32::from_rgb(104, 110, 123);
/// "Current" and "on".
pub const ACCENT: Color32 = Color32::from_rgb(52, 196, 176);
/// Text on an accent fill.
pub const ON_ACCENT: Color32 = Color32::from_rgb(6, 22, 19);
/// Accent at low strength, for selected rows.
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(22, 52, 50);
pub const GOOD: Color32 = Color32::from_rgb(70, 214, 112);
pub const WARN: Color32 = Color32::from_rgb(244, 186, 72);
pub const BAD: Color32 = Color32::from_rgb(238, 82, 82);
/// The raw picture mode's colour (the honest picture, nothing cleaned).
pub const RAW: Color32 = Color32::from_rgb(244, 186, 72);

/// Backing behind text drawn on the picture.
pub const SCRIM: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 178);
/// Opaque-enough sheet over the picture (the menu, the switch strip).
pub const SHEET: Color32 = Color32::from_rgba_premultiplied(15, 17, 21, 246);

pub const RADIUS: u8 = 3;
pub const R: CornerRadius = CornerRadius::same(RADIUS);

/// Text sizes.
pub const SIZE_BODY: f32 = 14.0;
pub const SIZE_SMALL: f32 = 12.0;
pub const SIZE_LABEL: f32 = 11.0;
pub const SIZE_TITLE: f32 = 18.0;

pub fn body() -> FontId {
    FontId::proportional(SIZE_BODY)
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Applies the style to every theme egui knows (so the OS's light/dark
/// setting cannot switch the app to a light look).
pub fn apply(ctx: &egui::Context) {
    load_font(ctx);
    ctx.all_styles_mut(|s| {
        s.animation_time = 0.05;
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 4.0);
        s.spacing.interact_size = Vec2::new(36.0, 24.0);
        s.spacing.window_margin = egui::Margin::same(14);
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.slider_width = 150.0;
        s.spacing.combo_width = 120.0;
        s.spacing.indent = 14.0;
        use egui::TextStyle as T;
        s.text_styles.insert(T::Body, body());
        s.text_styles.insert(T::Button, body());
        s.text_styles.insert(T::Small, FontId::proportional(SIZE_SMALL));
        s.text_styles.insert(T::Heading, FontId::proportional(SIZE_TITLE));
        s.text_styles.insert(T::Monospace, mono(13.0));

        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = SURFACE;
        v.window_fill = SURFACE;
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(4);
        v.menu_corner_radius = R;
        v.window_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: Color32::from_black_alpha(100) };
        v.extreme_bg_color = BG;
        v.faint_bg_color = Color32::from_rgb(23, 25, 31);
        v.code_bg_color = RAISED;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = WARN;
        v.error_fg_color = BAD;
        v.selection.bg_fill = ACCENT_SOFT;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.striped = true;
        v.slider_trailing_fill = true;
        v.collapsing_header_frame = false;
        v.indent_has_left_vline = false;
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = SURFACE;
        w.noninteractive.weak_bg_fill = SURFACE;
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        w.noninteractive.fg_stroke = Stroke::new(1.0, DIM);
        w.noninteractive.corner_radius = R;
        for (wv, fill, fg) in [
            (&mut w.inactive, RAISED, TEXT),
            (&mut w.hovered, HOVER, TEXT),
            (&mut w.active, ACCENT_SOFT, TEXT),
            (&mut w.open, HOVER, TEXT),
        ] {
            wv.bg_fill = fill;
            wv.weak_bg_fill = fill;
            wv.bg_stroke = Stroke::NONE;
            wv.fg_stroke = Stroke::new(1.0, fg);
            wv.corner_radius = R;
            wv.expansion = 0.0;
        }
        w.hovered.bg_stroke = Stroke::new(1.0, LINE);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    });
}

/// UI font: the system's regular-weight sans when one is found (egui's
/// built-in Ubuntu Light is thin and hard to read over video), egui's own
/// otherwise. `OPENVRX_UI_FONT=FILE` picks another; `OPENVRX_UI_FONT=builtin`
/// keeps egui's. The monospace font (numbers in the OSD) stays egui's Hack.
fn load_font(ctx: &egui::Context) {
    let chosen = std::env::var_os("OPENVRX_UI_FONT");
    if chosen.as_deref() == Some(std::ffi::OsStr::new("builtin")) {
        return;
    }
    let mut candidates: Vec<std::path::PathBuf> = chosen.into_iter().map(Into::into).collect();
    if let Some(win) = std::env::var_os("WINDIR") {
        let fonts = std::path::PathBuf::from(win).join("Fonts");
        candidates.push(fonts.join("segoeui.ttf"));
    }
    for p in [
        "/usr/share/fonts/adwaita-sans-fonts/AdwaitaSans-Regular.ttf",
        "/usr/share/fonts/inter/Inter-Regular.otf",
        "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf",
    ] {
        candidates.push(p.into());
    }
    let Some((path, bytes)) = candidates.iter().find_map(|p| std::fs::read(p).ok().map(|b| (p, b))) else {
        return;
    };
    log::info!("UI font: {}", path.display());
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("ui".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    // First in the proportional family; egui's fonts stay behind it for the
    // glyphs it lacks (emoji, arrows).
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    ctx.set_fonts(fonts);
}

/// Small caps-style section title.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title.to_uppercase()).size(SIZE_LABEL).color(FAINT).extra_letter_spacing(0.8));
}

/// A flat button: accent-filled when `on`.
pub fn toggle_button(text: impl Into<String>, on: bool) -> egui::Button<'static> {
    let (fill, color) = if on { (ACCENT, ON_ACCENT) } else { (RAISED, TEXT) };
    egui::Button::new(egui::RichText::new(text.into()).color(color))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(R)
}

/// A primary action button (accent outline).
pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.into()).color(TEXT))
        .fill(ACCENT_SOFT)
        .stroke(Stroke::new(1.0, ACCENT))
        .corner_radius(R)
}

/// Text on the picture with a dark backing; returns the backing's rect.
pub fn plate(p: &Painter, pos: Pos2, anchor: Align2, text: &str, font: FontId, color: Color32) -> Rect {
    let galley = p.layout_no_wrap(text.to_string(), font, color);
    let pad = Vec2::new(5.0, 2.0);
    let rect = anchor.anchor_size(pos, galley.size() + 2.0 * pad);
    p.rect_filled(rect, CornerRadius::same(2), SCRIM);
    p.galley(rect.min + pad, galley, color);
    rect
}

/// A key cap ("F11", "Ctrl+Q") placed by `anchor` at `pos`.
pub fn key_cap(p: &Painter, pos: Pos2, anchor: Align2, key: &str) -> Rect {
    let galley = p.layout_no_wrap(key.to_string(), mono(11.5), DIM);
    let size = Vec2::new(galley.size().x + 12.0, 19.0);
    let rect = anchor.anchor_size(pos, size);
    p.rect(rect, CornerRadius::same(2), RAISED, Stroke::new(1.0, LINE), StrokeKind::Inside);
    p.galley(rect.center() - galley.size() / 2.0, galley, DIM);
    rect
}

/// Colour of a picture mode in the badge and the strip: amber for raw (the
/// honest picture), the accent for every cleaned mode.
pub fn mode_color(raw: bool) -> Color32 {
    if raw {
        RAW
    } else {
        ACCENT
    }
}
