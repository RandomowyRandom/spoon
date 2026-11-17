//! Material 3 Expressive design tokens: dynamic color from a seed, type, shape,
//! and the egui style derived from them.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Id, Stroke};
use material_colors::color::Rgb;
use material_colors::dynamic_color::{SpecVersion, Variant};
use material_colors::hct::Hct;
use material_colors::theme::ThemeBuilder;
use serde::{Deserialize, Serialize};

use crate::graph::LANE_COLORS;

// ---------------------------------------------------------------- preferences

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    System,
    Light,
    Dark,
}

/// The palette styles Android offers under "Wallpaper & style".
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaletteStyle {
    TonalSpot,
    Vibrant,
    Expressive,
    Neutral,
    Rainbow,
    FruitSalad,
    Monochrome,
}

impl PaletteStyle {
    pub const ALL: [PaletteStyle; 7] = [
        PaletteStyle::TonalSpot,
        PaletteStyle::Vibrant,
        PaletteStyle::Expressive,
        PaletteStyle::Neutral,
        PaletteStyle::Rainbow,
        PaletteStyle::FruitSalad,
        PaletteStyle::Monochrome,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PaletteStyle::TonalSpot => "Tonal spot",
            PaletteStyle::Vibrant => "Vibrant",
            PaletteStyle::Expressive => "Expressive",
            PaletteStyle::Neutral => "Neutral",
            PaletteStyle::Rainbow => "Rainbow",
            PaletteStyle::FruitSalad => "Fruit salad",
            PaletteStyle::Monochrome => "Monochrome",
        }
    }

    fn variant(self) -> Variant {
        match self {
            PaletteStyle::TonalSpot => Variant::TonalSpot,
            PaletteStyle::Vibrant => Variant::Vibrant,
            PaletteStyle::Expressive => Variant::Expressive,
            PaletteStyle::Neutral => Variant::Neutral,
            PaletteStyle::Rainbow => Variant::Rainbow,
            PaletteStyle::FruitSalad => Variant::FruitSalad,
            PaletteStyle::Monochrome => Variant::Monochrome,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemePrefs {
    pub mode: Mode,
    pub seed: [u8; 3],
    pub style: PaletteStyle,
    /// -1 (reduced) ..= 1 (high); 0 is standard.
    pub contrast: f32,
}

impl Default for ThemePrefs {
    fn default() -> Self {
        Self { mode: Mode::System, seed: SEEDS[0], style: PaletteStyle::TonalSpot, contrast: 0.0 }
    }
}

/// Preset source colors, like the "Basic colors" row on Android.
pub const SEEDS: [[u8; 3]; 8] = [
    [0x5B, 0x5F, 0xE9], // indigo
    [0x1B, 0x6E, 0xF3], // blue
    [0x00, 0x8F, 0x7A], // teal
    [0x3F, 0x8F, 0x2F], // green
    [0xC9, 0x9A, 0x06], // amber
    [0xE8, 0x6A, 0x17], // orange
    [0xD8, 0x2C, 0x6B], // rose
    [0x9A, 0x4D, 0xD6], // violet
];

// ---------------------------------------------------------------- color roles

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub dark: bool,
    pub primary: Color32,
    pub on_primary: Color32,
    pub primary_container: Color32,
    pub on_primary_container: Color32,
    pub secondary: Color32,
    pub secondary_container: Color32,
    pub on_secondary_container: Color32,
    pub tertiary: Color32,
    pub on_tertiary: Color32,
    pub tertiary_container: Color32,
    pub on_tertiary_container: Color32,
    pub error: Color32,
    pub on_error: Color32,
    pub error_container: Color32,
    pub on_error_container: Color32,
    pub surface: Color32,
    pub surface_low: Color32,
    pub surface_container: Color32,
    pub surface_high: Color32,
    pub surface_highest: Color32,
    pub on_surface: Color32,
    pub on_surface_variant: Color32,
    pub outline: Color32,
    pub outline_variant: Color32,
    pub inverse_surface: Color32,
    pub inverse_on_surface: Color32,
    pub inverse_primary: Color32,
    pub scrim: Color32,
    /// Commit graph lanes: hues spread around the primary hue.
    pub lanes: [Color32; LANE_COLORS],
    pub diff_added: Color32,
    pub diff_deleted: Color32,
    pub added: Color32,
    pub deleted: Color32,
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c.red, c.green, c.blue)
}

fn hct(hue: f64, chroma: f64, tone: f64) -> Color32 {
    rgb(Rgb::from(Hct::from(hue.rem_euclid(360.0), chroma, tone)))
}

impl Colors {
    pub fn build(prefs: &ThemePrefs, system_dark: bool) -> Self {
        let dark = match prefs.mode {
            Mode::System => system_dark,
            Mode::Light => false,
            Mode::Dark => true,
        };
        let [r, g, b] = prefs.seed;
        let schemes = ThemeBuilder::with_source(Rgb::new(r, g, b))
            .variant(prefs.style.variant())
            .spec_version(SpecVersion::Spec2025)
            .contrast_level(f64::from(prefs.contrast.clamp(-1.0, 1.0)))
            .build()
            .schemes;
        let s = if dark { schemes.dark } else { schemes.light };

        let primary_hue = Hct::new(s.primary).get_hue();
        let lane_tone = if dark { 74.0 } else { 48.0 };
        let offsets = [0.0, 180.0, 90.0, 270.0, 45.0, 225.0, 135.0, 315.0];
        let (bg_tone, fg_tone) = if dark { (17.0, 80.0) } else { (94.0, 40.0) };

        Self {
            dark,
            primary: rgb(s.primary),
            on_primary: rgb(s.on_primary),
            primary_container: rgb(s.primary_container),
            on_primary_container: rgb(s.on_primary_container),
            secondary: rgb(s.secondary),
            secondary_container: rgb(s.secondary_container),
            on_secondary_container: rgb(s.on_secondary_container),
            tertiary: rgb(s.tertiary),
            on_tertiary: rgb(s.on_tertiary),
            tertiary_container: rgb(s.tertiary_container),
            on_tertiary_container: rgb(s.on_tertiary_container),
            error: rgb(s.error),
            on_error: rgb(s.on_error),
            error_container: rgb(s.error_container),
            on_error_container: rgb(s.on_error_container),
            surface: rgb(s.surface),
            surface_low: rgb(s.surface_container_low),
            surface_container: rgb(s.surface_container),
            surface_high: rgb(s.surface_container_high),
            surface_highest: rgb(s.surface_container_highest),
            on_surface: rgb(s.on_surface),
            on_surface_variant: rgb(s.on_surface_variant),
            outline: rgb(s.outline),
            outline_variant: rgb(s.outline_variant),
            inverse_surface: rgb(s.inverse_surface),
            inverse_on_surface: rgb(s.inverse_on_surface),
            inverse_primary: rgb(s.inverse_primary),
            scrim: rgb(s.scrim),
            lanes: offsets.map(|o| hct(primary_hue + o, 58.0, lane_tone)),
            diff_added: hct(148.0, 20.0, bg_tone),
            diff_deleted: hct(22.0, 22.0, bg_tone),
            added: hct(148.0, 55.0, fg_tone),
            deleted: hct(22.0, 65.0, fg_tone),
        }
    }

    /// State layer: `content` over `base` at the given opacity.
    pub fn state(base: Color32, content: Color32, opacity: f32) -> Color32 {
        base.lerp_to_gamma(content, opacity)
    }
}

fn store_id() -> Id {
    Id::unique("spoon-m3-colors")
}

pub fn colors(ctx: &egui::Context) -> Colors {
    ctx.data(|d| d.get_temp(store_id())).unwrap_or_else(|| Colors::build(&ThemePrefs::default(), true))
}

// ---------------------------------------------------------------- shape

/// M3 Expressive corner scale.
pub mod shape {
    pub const SMALL: f32 = 8.0;
    pub const MEDIUM: f32 = 12.0;
    pub const LARGE: f32 = 16.0;
    pub const LARGE_INCREASED: f32 = 20.0;
    pub const EXTRA_LARGE: f32 = 28.0;
}

// ---------------------------------------------------------------- type

const EMPHASIZED: &str = "emphasized";

pub fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Emphasized (semibold) text: titles, labels, selected items.
pub fn emph(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(EMPHASIZED.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn icon(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(egui_material_icons::FONT_FAMILY.into()))
}

/// Platform UI font (regular or semibold): Segoe UI on Windows, fontconfig's
/// default sans elsewhere. `None` falls back to egui's bundled font.
fn system_font(emphasized: bool) -> Option<FontData> {
    #[cfg(windows)]
    {
        let file = if emphasized { "seguisb.ttf" } else { "segoeui.ttf" };
        let path = std::path::PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts").join(file);
        std::fs::read(path).ok().map(FontData::from_owned)
    }
    #[cfg(not(windows))]
    {
        let pattern = if emphasized { "sans-serif:demibold" } else { "sans-serif:regular" };
        let out = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}", pattern]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (file, index) = text.split_once('\n')?;
        let mut data = FontData::from_owned(std::fs::read(file.trim()).ok()?);
        data.index = index.trim().parse().unwrap_or(0);
        Some(data)
    }
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mono = FontData::from_static(include_bytes!("../assets/fonts/JetBrainsMonoNL-Regular.ttf"));
    fonts.font_data.insert("mono".into(), Arc::new(mono));
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());

    let mut emphasized = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(regular) = system_font(false) {
        fonts.font_data.insert("ui".into(), Arc::new(regular));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
        emphasized.insert(0, "ui".into());
    }
    if let Some(bold) = system_font(true) {
        fonts.font_data.insert("ui-emphasized".into(), Arc::new(bold));
        emphasized.insert(0, "ui-emphasized".into());
    }
    fonts.families.insert(FontFamily::Name(EMPHASIZED.into()), emphasized);
    ctx.set_fonts(fonts);
    egui_material_icons::initialize(ctx);
}

// ---------------------------------------------------------------- egui style

/// Publishes the colors and restyles the stock egui widgets we still use
/// (text edits, scroll bars, menus, tooltips, color picker).
pub fn apply(ctx: &egui::Context, c: &Colors) {
    ctx.data_mut(|d| d.insert_temp(store_id(), *c));
    ctx.set_theme(if c.dark { egui::Theme::Dark } else { egui::Theme::Light });
    let c = *c;
    ctx.global_style_mut(move |style| {
        let radius = CornerRadius::same(shape::MEDIUM as u8);
        let v = &mut style.visuals;
        v.dark_mode = c.dark;
        v.override_text_color = None;
        v.panel_fill = c.surface_container;
        v.window_fill = c.surface_high;
        v.window_stroke = Stroke::NONE;
        v.window_corner_radius = CornerRadius::same(shape::EXTRA_LARGE as u8);
        v.menu_corner_radius = CornerRadius::same(shape::LARGE as u8);
        v.extreme_bg_color = c.surface_highest;
        v.text_edit_bg_color = Some(c.surface_highest);
        v.faint_bg_color = c.surface_low;
        v.code_bg_color = c.surface_highest;
        v.hyperlink_color = c.primary;
        v.error_fg_color = c.error;
        v.warn_fg_color = c.tertiary;
        v.selection.bg_fill = c.primary.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, c.primary);
        v.text_cursor.stroke = Stroke::new(2.0, c.primary);
        let shadow = egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if c.dark { 110 } else { 45 }) };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;

        let hover = Colors::state(c.surface_highest, c.on_surface, 0.08);
        let w = &mut v.widgets;
        for (visuals, fill, fg) in [
            (&mut w.noninteractive, c.surface_high, c.on_surface),
            (&mut w.inactive, c.surface_highest, c.on_surface_variant),
            (&mut w.hovered, hover, c.on_surface),
            (&mut w.active, c.secondary_container, c.on_secondary_container),
            (&mut w.open, c.secondary_container, c.on_secondary_container),
        ] {
            visuals.bg_fill = fill;
            visuals.weak_bg_fill = fill;
            visuals.bg_stroke = Stroke::NONE;
            visuals.fg_stroke = Stroke::new(1.0, fg);
            visuals.corner_radius = radius;
            visuals.expansion = 0.0;
        }
        w.noninteractive.bg_stroke = Stroke::new(1.0, c.outline_variant);

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.window_margin = egui::Margin::same(24);
        style.spacing.scroll.floating = true;
        style.spacing.scroll.bar_width = 8.0;
        style.animation_time = 0.2;

        use egui::TextStyle::*;
        style.text_styles = [
            (Small, body(12.0)),
            (Body, body(14.0)),
            (Button, emph(14.0)),
            (Heading, emph(22.0)),
            (Monospace, mono(13.0)),
        ]
        .into();
    });
}
