//! Tokens from Paper's "Wiesel · Switcher (dark/light)" artboards and the shadcn/ui
//! neutral theme: a cmd+tab-style glass HUD of icon tiles, and quiet neutral windows.
//! The palette follows the Appearance setting (Auto tracks macOS); `set_appearance` is called
//! before every frame.
use crate::settings::Appearance;
use gpui::{prelude::*, *};
use std::{
    borrow::Cow,
    sync::atomic::{AtomicBool, Ordering},
};

/// Every themed color, as 0xRRGGBB (or 0xRRGGBBAA where noted).
pub struct Palette {
    pub dark: bool,
    // Windows and content surfaces (shadcn/ui neutral).
    pub background: u32,
    pub foreground: u32,
    pub muted: u32,
    pub card: u32,
    pub field: u32,
    /// Hover/selected fill for ghost controls.
    pub accent: u32,
    /// Solid primary button fill and its label.
    pub primary: u32,
    pub primary_foreground: u32,
    pub destructive: u32,
    pub ring: u32,
    pub paper: u32,
    pub paper_border: u32,
    pub status_label: u32,
    /// 0xRRGGBBAA hairlines.
    pub border: u32,
    /// 0xRRGGBBAA field outlines.
    pub input_border: u32,
    /// 0xRRGGBBAA 1px highlight under raised surfaces.
    pub edge_highlight: u32,
    // User message bubble.
    pub bubble_hi: u32,
    pub bubble: u32,
    pub bubble_edge: u32,
    pub bubble_text: u32,
    pub bubble_meta: u32,
    // The launcher HUD. Tints sit on top of the window's native blur.
    /// 0xRRGGBBAA glass tint.
    pub hud_tint: u32,
    /// Icon tile faces (top, bottom), 0xRRGGBBAA rim and printed glyph.
    pub tile_face: (u32, u32),
    pub tile_rim: u32,
    pub tile_ink: u32,
    /// 0xRRGGBBAA soft hover fill for small launcher controls.
    pub tile_selected: u32,
    /// 0xRRGGBBAA halo around the selected icon tile.
    pub tile_halo: u32,
    pub tile_label: u32,
    pub tile_label_selected: u32,
    pub tile_number: u32,
    /// 0xRRGGBBAA status strip fill and top hairline.
    pub status_fill: u32,
    pub status_edge: u32,
    pub status_detail: u32,
    /// 0xRRGGBBAA keycap fill.
    pub kbd: u32,
    pub kbd_text: u32,
}

pub const LIGHT: Palette = Palette {
    dark: false,
    background: 0xffffff,
    foreground: 0x0a0a0a,
    muted: 0x737373,
    card: 0xffffff,
    field: 0xffffff,
    accent: 0xf5f5f5,
    primary: 0x171717,
    primary_foreground: 0xfafafa,
    destructive: 0xdc2626,
    ring: 0xa3a3a3,
    paper: 0xffffff,
    paper_border: 0xe5e5e5,
    status_label: 0x171717,
    border: 0x0000001a,
    input_border: 0x00000024,
    edge_highlight: 0xffffffcc,
    bubble_hi: 0xf5f5f5,
    bubble: 0xf0f0f0,
    bubble_edge: 0xe5e5e5,
    bubble_text: 0x0a0a0a,
    bubble_meta: 0x737373,
    hud_tint: 0xf6f6f8e6,
    tile_face: (0xffffff, 0xf2f2f2),
    tile_rim: 0x00000017,
    tile_ink: 0x171717,
    tile_selected: 0x00000012,
    tile_halo: 0x00000024,
    tile_label: 0x424242,
    tile_label_selected: 0x0a0a0a,
    tile_number: 0x8a8a8a,
    status_fill: 0x00000009,
    status_edge: 0x0000000f,
    status_detail: 0x636363,
    kbd: 0xffffffcc,
    kbd_text: 0x404040,
};

pub const DARK: Palette = Palette {
    dark: true,
    background: 0x0a0a0a,
    foreground: 0xfafafa,
    muted: 0xa1a1a1,
    card: 0x171717,
    field: 0x141414,
    accent: 0x262626,
    primary: 0xe5e5e5,
    primary_foreground: 0x171717,
    destructive: 0xf87171,
    ring: 0x737373,
    paper: 0x171717,
    paper_border: 0x2a2a2a,
    status_label: 0xd9d9d9,
    border: 0xffffff1a,
    input_border: 0xffffff26,
    edge_highlight: 0xffffff0f,
    bubble_hi: 0x2a2a2a,
    bubble: 0x262626,
    bubble_edge: 0x333333,
    bubble_text: 0xfafafa,
    bubble_meta: 0xa1a1a1,
    hud_tint: 0x1e1e20eb,
    tile_face: (0x383838, 0x1f1f1f),
    tile_rim: 0xffffff10,
    tile_ink: 0xfafafa,
    tile_selected: 0xffffff21,
    tile_halo: 0xffffff38,
    tile_label: 0xb8b8b8,
    tile_label_selected: 0xfafafa,
    tile_number: 0x7a7a7a,
    status_fill: 0x0000002e,
    status_edge: 0xffffff12,
    status_detail: 0x8f8f8f,
    kbd: 0xffffff12,
    kbd_text: 0xc7c7c7,
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// Apply the user's preference, resolving Auto against the window's effective macOS
/// appearance. Returns true when it changed.
pub fn set_appearance(preference: Appearance, system: WindowAppearance) -> bool {
    let dark = match preference {
        Appearance::Auto => matches!(
            system,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
        Appearance::Light => false,
        Appearance::Dark => true,
    };
    DARK_MODE.swap(dark, Ordering::Relaxed) != dark
}

/// The active palette.
pub fn p() -> &'static Palette {
    if DARK_MODE.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

// Appearance-independent status colors (macOS system green/orange/red, accent blue).
pub const LED_GREEN: u32 = 0x34c759;
pub const LED_AMBER: u32 = 0xff9f0a;
pub const LED_RED: u32 = 0xff453a;
pub const LED_BLUE: u32 = 0x0a84ff;

/// The macOS UI font (SF Pro), so every weight renders natively.
pub const SANS: &str = ".SystemUIFont";
/// Keycaps and pad numbers.
pub const MONO: &str = "DM Mono";

/// CSS-ordered shadow helper: `x y blur spread color`, colors as 0xRRGGBBAA.
pub fn shadow(x: f32, y: f32, blur: f32, spread: f32, color: u32) -> BoxShadow {
    BoxShadow::new(px(x), px(y), rgba(color).into())
        .blur_radius(px(blur))
        .spread_radius(px(spread))
}
fn inset(x: f32, y: f32, blur: f32, spread: f32, color: u32) -> BoxShadow {
    shadow(x, y, blur, spread, color).inset()
}
pub fn vertical(top: u32, bottom: u32, split: f32) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(rgb(top), 0.),
        linear_color_stop(rgb(bottom), split),
    )
}
/// Focus ring shared by every control: 3px of the ring color at half strength.
fn focus_ring(base: Vec<BoxShadow>) -> Vec<BoxShadow> {
    let mut ring = base;
    ring.insert(0, shadow(0., 0., 0., 3., (p().ring << 8) | 0x80));
    ring
}

/// shadcn/ui button variants.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Outline button: the default.
    Grey,
    /// Solid primary button.
    Blue,
    /// Destructive confirmation.
    Red,
}
impl Key {
    /// (fill, hover fill, border 0xRRGGBBAA, label)
    fn colors(self) -> (u32, u32, u32, u32) {
        let palette = p();
        match self {
            Self::Grey => (
                palette.background,
                palette.accent,
                palette.input_border,
                palette.foreground,
            ),
            Self::Blue => (
                palette.primary,
                if palette.dark { 0xd4d4d4 } else { 0x2e2e2e },
                (palette.primary << 8) | 0xff,
                palette.primary_foreground,
            ),
            Self::Red => (
                palette.destructive,
                if palette.dark { 0xef6060 } else { 0xc81e1e },
                (palette.destructive << 8) | 0xff,
                0xffffff,
            ),
        }
    }
    pub fn label(self) -> u32 {
        self.colors().3
    }
}

/// Styles any stateful element as a shadcn/ui button.
pub fn key<E: Styled + StatefulInteractiveElement>(element: E, kind: Key) -> E {
    let (fill, hover, border, label) = kind.colors();
    let rest = vec![shadow(0., 1., 2., 0., 0x0000000d)];
    let ring = focus_ring(rest.clone());
    element
        .relative()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(rgba(border))
        .bg(rgb(fill))
        .shadow(rest)
        .text_color(rgb(label))
        .font_family(SANS)
        .font_weight(FontWeight::MEDIUM)
        .text_size(px(12.5))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(hover)))
        .active(move |s| s.opacity(0.85))
        .focus_visible(move |s| s.shadow(ring))
}

/// The launcher's icon square: a softly lit rounded tile, like an app icon in cmd+tab.
/// Selection is a halo hugging the tile; its spread follows the tile's corners.
pub fn tile<E: Styled + StatefulInteractiveElement>(element: E, selected: bool) -> E {
    let palette = p();
    let mut rest = vec![
        inset(
            0.,
            1.,
            0.,
            0.,
            if palette.dark { 0xffffff24 } else { 0xffffffff },
        ),
        inset(0., 0., 0., 1., palette.tile_rim),
        shadow(
            0.,
            1.,
            2.,
            0.,
            if palette.dark { 0x00000080 } else { 0x0000001a },
        ),
        shadow(
            0.,
            6.,
            14.,
            0.,
            if palette.dark { 0x00000040 } else { 0x0000000f },
        ),
    ];
    if selected {
        rest.insert(0, shadow(0., 0., 0., 3., palette.tile_halo));
    }
    let ring = focus_ring(rest.clone());
    element
        .relative()
        .rounded(px(18.))
        .bg(vertical(palette.tile_face.0, palette.tile_face.1, 1.))
        .shadow(rest)
        // Hover and press feedback belong to the whole launcher slot, not the square.
        .focus_visible(move |s| s.shadow(ring))
}

/// Small keycap, e.g. `esc` in the status strip.
pub fn kbd(text: impl Into<SharedString>) -> Div {
    let palette = p();
    div()
        .flex_shrink_0()
        .h(px(16.))
        .px(px(5.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(rgba(palette.kbd))
        .shadow(vec![inset(0., 0., 0., 1., palette.border)])
        .font_family(MONO)
        .text_size(px(10.))
        .text_color(rgb(palette.kbd_text))
        .child(text.into())
}

/// Status dot with a soft halo.
pub fn led(color: u32) -> Div {
    div()
        .size(px(7.))
        .rounded_full()
        .flex_shrink_0()
        .bg(rgb(color))
        .shadow(vec![shadow(0., 0., 0., 3., (color << 8) | 0x29)])
}

/// Section heading: small, medium-weight muted text.
pub fn silkscreen(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .font_family(SANS)
        .text_size(px(12.))
        .line_height(px(16.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(color))
        .whitespace_nowrap()
        .child(text.into())
}

pub fn load_fonts(cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../resources/fonts/Inter.ttf")),
        Cow::Borrowed(include_bytes!("../resources/fonts/DMMono-Regular.ttf")),
    ]) {
        eprintln!("Unable to load Wiesel fonts; using system fallback: {error:#}");
    }
}

pub fn icon(name: &'static str, size: f32) -> Svg {
    // GPUI's SVG paint path checks the element's own color, not inherited text color.
    svg()
        .path(name)
        .size(px(size))
        .flex_shrink_0()
        .text_color(rgb(p().foreground))
}

// Embedded SVGs keep the packaged app independent of the working directory.
pub struct Assets;
const ICONS: &[(&str, &str)] = &[
    (
        "search",
        r#"<path d="m21 21-4.34-4.34"/><circle cx="11" cy="11" r="8"/>"#,
    ),
    (
        "spell-check",
        r#"<path d="m20 15-5.5 5.5L12 18M4 16l6-12 5.115 10.23M6 12h8"/>"#,
    ),
    (
        "wand",
        r#"<path d="m21.64 3.64-1.28-1.28a1.21 1.21 0 0 0-1.72 0L2.36 18.64a1.21 1.21 0 0 0 0 1.72l1.28 1.28a1.2 1.2 0 0 0 1.72 0L21.64 5.36a1.2 1.2 0 0 0 0-1.72ZM14 7l3 3M5 6v4M3 8h4M19 14v4M17 16h4M10 2v2M9 3h2"/>"#,
    ),
    (
        "chat",
        r#"<path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5Z"/>"#,
    ),
    ("list-filter", r#"<path d="M2 5h20M6 12h12M9 19h6"/>"#),
    (
        "lightbulb",
        r#"<path d="M15 14c.2-1 .7-1.7 1.5-2.5 1-.9 1.5-2.2 1.5-3.5A6 6 0 0 0 6 8c0 1 .2 2.2 1.5 3.5.7.7 1.3 1.5 1.5 2.5M9 18h6M10 22h4"/>"#,
    ),
    ("plus", r#"<path d="M5 12h14M12 5v14"/>"#),
    ("minus", r#"<path d="M5 12h14"/>"#),
    (
        "settings",
        r#"<path d="m9 3-.6 2.4-2.1 1.2L4 6l-2 3 1.7 1.8v2.4L2 15l2 3 2.3-.6 2.1 1.2L9 21h6l.6-2.4 2.1-1.2 2.3.6 2-3-1.7-1.8v-2.4L22 9l-2-3-2.3.6-2.1-1.2L15 3Z"/><circle cx="12" cy="12" r="3"/>"#,
    ),
    ("arrow-left", r#"<path d="m12 19-7-7 7-7M19 12H5"/>"#),
    (
        "rotate-ccw",
        r#"<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>"#,
    ),
    ("arrow-up", r#"<path d="m5 12 7-7 7 7M12 19V5"/>"#),
    // A quiet ring with a short arc riding on it: the launcher's single progress spinner.
    ("spinner-track", r#"<circle cx="12" cy="12" r="9"/>"#),
    ("spinner-arc", r#"<path d="M12 3a9 9 0 0 1 9 9"/>"#),
    (
        "copy",
        r#"<rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2"/>"#,
    ),
];
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        // App icons take precedence; any other Lucide icon may back a quick action.
        let body = ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, body)| *body)
            .or_else(|| crate::lucide::find(path).map(|icon| icon.body));
        Ok(body.map(|body| {
            Cow::Owned(format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#).into_bytes())
        }))
    }
    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .chain(
                crate::lucide::icons()
                    .iter()
                    .map(|icon| SharedString::from(icon.name)),
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, DARK, LIGHT, icon, p, set_appearance};
    use crate::settings::Appearance;
    use gpui::{AssetSource, Styled, WindowAppearance, rgb};

    #[test]
    fn icons_have_an_explicit_paint_color() {
        let mut svg = icon("plus", 24.);
        assert_eq!(svg.style().text.color, Some(rgb(p().foreground).into()));
    }

    #[test]
    fn icon_paint_color_can_be_overridden() {
        let mut svg = icon("plus", 24.).text_color(rgb(p().muted));
        assert_eq!(svg.style().text.color, Some(rgb(p().muted).into()));
    }
    #[test]
    fn palettes_differ_in_surfaces() {
        assert_ne!(LIGHT.background, DARK.background);
        assert_ne!(LIGHT.foreground, DARK.foreground);
        // Only assert the light default: other tests in this process render in Light.
        set_appearance(Appearance::Auto, WindowAppearance::Light);
        assert!(!p().dark);
        set_appearance(Appearance::Light, WindowAppearance::Dark);
        assert!(!p().dark);
    }
    #[test]
    fn every_listed_icon_is_embedded() {
        for name in Assets.list("").unwrap() {
            assert!(
                Assets.load(&name).unwrap().is_some(),
                "Missing icon: {name}"
            );
        }
    }
    #[test]
    fn unknown_asset_is_not_loaded() {
        assert!(Assets.load("missing").unwrap().is_none());
    }
}
