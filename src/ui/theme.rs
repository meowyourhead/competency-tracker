//! Palette, fonts and egui style: a warm, calm look (cream background,
//! sidebar, serif headings, terracotta accent, rounded cards and pills).

use egui::{Color32, CornerRadius, FontDefinitions, FontFamily, Margin, RichText, Shadow, Stroke, Visuals};

use crate::prefs::Theme;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub green_bg: Color32,
    pub green_fg: Color32,
    pub yellow_bg: Color32,
    pub yellow_fg: Color32,
    pub red_bg: Color32,
    pub red_fg: Color32,
    pub grey_bg: Color32,
    pub grey_fg: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub fn palette(t: Theme) -> Palette {
    match t {
        Theme::Light => Palette {
            bg: hex(0xFAF9F5),
            sidebar: hex(0xF0EEE6),
            card: hex(0xFFFFFF),
            border: hex(0xE8E6DC),
            text: hex(0x1F1E1D),
            muted: hex(0x73726C),
            accent: hex(0xC96442),
            green_bg: hex(0xE3EFE0),
            green_fg: hex(0x2F6B3A),
            yellow_bg: hex(0xF6EBC8),
            yellow_fg: hex(0x7A5A0B),
            red_bg: hex(0xF6DEDA),
            red_fg: hex(0x9B2C1F),
            grey_bg: hex(0xEDEBE4),
            grey_fg: hex(0x73726C),
        },
        Theme::Dark => Palette {
            bg: hex(0x262624),
            sidebar: hex(0x1F1E1D),
            card: hex(0x30302E),
            border: hex(0x3E3E38),
            text: hex(0xFAF9F5),
            muted: hex(0xA6A39A),
            accent: hex(0xD97757),
            green_bg: hex(0x2C3B2D),
            green_fg: hex(0x9CCB9F),
            yellow_bg: hex(0x3F3622),
            yellow_fg: hex(0xE3C077),
            red_bg: hex(0x45292A),
            red_fg: hex(0xF0A69B),
            grey_bg: hex(0x3A3935),
            grey_fg: hex(0xA6A39A),
        },
    }
}

/// The serif family used for page titles and headings.
pub fn serif() -> FontFamily {
    FontFamily::Name("serif".into())
}

/// Registers the fonts: proportional UI text, a `serif` family for headings,
/// and the Phosphor icons merged into the proportional family.
///
/// For now both families use egui's built-in fonts. To embed the intended
/// open-licence fonts, add these files to `assets/fonts/` and register them in
/// [`add_text_fonts`] only: `Inter-Regular.ttf`, `Inter-SemiBold.ttf`,
/// `SourceSerif4-Regular.ttf`, `SourceSerif4-Semibold.ttf`.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    add_text_fonts(&mut fonts);
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}

/// Sets up the proportional and serif families. Today the serif family is an
/// alias of the default proportional fonts.
fn add_text_fonts(fonts: &mut FontDefinitions) {
    let proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.insert(serif(), proportional);
}

/// Applies the palette and shapes for `t` and makes it the active theme.
pub fn apply(ctx: &egui::Context, t: Theme) {
    let pal = palette(t);
    let (egui_theme, mut v) = match t {
        Theme::Light => (egui::Theme::Light, Visuals::light()),
        Theme::Dark => (egui::Theme::Dark, Visuals::dark()),
    };
    let radius = CornerRadius::same(10);
    let border = Stroke::new(1.0, pal.border);
    let shadow_alpha = if t == Theme::Light { 22 } else { 90 };

    v.panel_fill = pal.bg;
    v.window_fill = pal.card;
    v.window_stroke = border;
    v.window_corner_radius = radius;
    v.menu_corner_radius = radius;
    v.window_shadow = Shadow { offset: [0, 6], blur: 24, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 12, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.extreme_bg_color = pal.card;
    v.faint_bg_color = pal.sidebar;
    v.code_bg_color = pal.sidebar;
    v.hyperlink_color = pal.accent;
    v.error_fg_color = pal.red_fg;
    v.warn_fg_color = pal.yellow_fg;
    v.selection.bg_fill = pal.accent.gamma_multiply(0.25);
    v.selection.stroke = Stroke::new(1.0, pal.accent);

    let hover_fill = pal.border;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = pal.card;
    w.noninteractive.weak_bg_fill = pal.card;
    w.noninteractive.bg_stroke = border;
    w.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);
    w.inactive.bg_fill = pal.card;
    w.inactive.weak_bg_fill = pal.card;
    w.inactive.bg_stroke = border;
    w.inactive.fg_stroke = Stroke::new(1.0, pal.text);
    w.hovered.bg_fill = hover_fill;
    w.hovered.weak_bg_fill = hover_fill;
    w.hovered.bg_stroke = Stroke::new(1.0, pal.muted.gamma_multiply(0.6));
    w.hovered.fg_stroke = Stroke::new(1.0, pal.text);
    w.active.bg_fill = hover_fill;
    w.active.weak_bg_fill = hover_fill;
    w.active.bg_stroke = Stroke::new(1.0, pal.accent);
    w.active.fg_stroke = Stroke::new(1.0, pal.text);
    w.open.bg_fill = pal.card;
    w.open.weak_bg_fill = pal.card;
    w.open.bg_stroke = border;
    w.open.fg_stroke = Stroke::new(1.0, pal.text);
    for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        wv.corner_radius = radius;
        wv.expansion = 0.0;
    }

    ctx.style_mut_of(egui_theme, |s| {
        s.visuals = v;
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.window_margin = Margin::same(12);
        s.spacing.menu_margin = Margin::same(8);
        s.spacing.interact_size.y = 30.0;
    });
    ctx.set_theme(egui_theme);
}

/// A page title: serif, 24 pt.
pub fn heading(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).family(serif()).size(24.0))
}
