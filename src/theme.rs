//! Neutral dark tokens from Paper's Wiesel · Home and Wiesel · Chat artboards.
use gpui::{prelude::*, *};
use std::borrow::Cow;

pub const BACKGROUND: u32 = 0x0a0a0a;
pub const FOREGROUND: u32 = 0xfafafa;
pub const CARD: u32 = 0x171717;
pub const ACCENT: u32 = 0x262626;
pub const MUTED: u32 = 0xa3a3a3;
pub const PRIMARY: u32 = 0xe5e5e5;
pub const RING: u32 = 0x737373;
pub const BORDER: u32 = 0xffffff1a;
pub const INPUT_BORDER: u32 = 0xffffff26;
pub const SANS: &str = "Inter";
pub const MONO: &str = "DM Mono";

pub fn load_fonts(cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../resources/fonts/Inter.ttf")),
        Cow::Borrowed(include_bytes!("../resources/fonts/DMMono-Regular.ttf")),
    ]) {
        eprintln!("Unable to load Wiesel fonts; using system fallback: {error:#}");
    }
}

pub fn badge(text: &str) -> Div {
    div()
        .min_w(px(20.))
        .h(px(20.))
        .px(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(rgba(BORDER))
        .font_family(MONO)
        .text_size(px(10.))
        .line_height(px(12.))
        .text_color(rgb(MUTED))
        .child(text.to_owned())
}

pub fn icon(name: &'static str, size: f32) -> Svg {
    // GPUI's SVG paint path checks the element's own color, not inherited text color.
    svg()
        .path(name)
        .size(px(size))
        .flex_shrink_0()
        .text_color(rgb(FOREGROUND))
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
    ("arrow-up", r#"<path d="m5 12 7-7 7 7M12 19V5"/>"#),
    (
        "copy",
        r#"<rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2"/>"#,
    ),
];
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(name, _)| *name == path).map(|(_, body)| {
            Cow::Owned(format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#).into_bytes())
        }))
    }
    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, FOREGROUND, MUTED, icon};
    use gpui::{AssetSource, Styled, rgb};

    #[test]
    fn icons_have_an_explicit_paint_color() {
        let mut svg = icon("plus", 24.);
        assert_eq!(svg.style().text.color, Some(rgb(FOREGROUND).into()));
    }

    #[test]
    fn icon_paint_color_can_be_overridden() {
        let mut svg = icon("plus", 24.).text_color(rgb(MUTED));
        assert_eq!(svg.style().text.color, Some(rgb(MUTED).into()));
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
