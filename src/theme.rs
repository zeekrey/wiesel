//! Tokens from Paper's "Wiesel MPC · Clean A (light)" and "Clean B (dark)" artboards:
//! a quiet chassis, a slim header with the wordmark and a status LED, and backlit rubber pads.
//! The palette follows the macOS appearance; `set_appearance` is called before every frame.
use gpui::{prelude::*, *};
use std::{
    borrow::Cow,
    sync::atomic::{AtomicBool, Ordering},
};

/// Every themed color, as 0xRRGGBB (or 0xRRGGBBAA where noted).
pub struct Palette {
    pub dark: bool,
    // Chassis and header.
    pub chassis_hi: u32,
    pub chassis: u32,
    pub header: u32,
    pub header_hi: u32,
    pub header_edge: u32,
    pub status_label: u32,
    // Rubber pads.
    pub empty_pad: u32,
    pub empty_pad_hi: u32,
    pub empty_pad_edge: u32,
    pub pad_number: u32,
    pub pad_title: u32,
    pub add_icon: u32,
    // Content surfaces.
    pub foreground: u32,
    pub muted: u32,
    pub paper: u32,
    pub paper_border: u32,
    pub card: u32,
    pub field: u32,
    pub accent: u32,
    pub primary: u32,
    pub ring: u32,
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
}

pub const LIGHT: Palette = Palette {
    dark: false,
    chassis_hi: 0xf1efe9,
    chassis: 0xe5e2da,
    header: 0xf6f4ef,
    header_hi: 0xf0eee8,
    header_edge: 0xd3cfc5,
    status_label: 0x6f6d68,
    empty_pad: 0xdcd9d1,
    empty_pad_hi: 0xe0ddd5,
    empty_pad_edge: 0xc9c5bc,
    pad_number: 0x9a978f,
    pad_title: 0x26262a,
    add_icon: 0x5a9fd6,
    foreground: 0x1e2228,
    muted: 0x5e5a52,
    paper: 0xf6f3ec,
    paper_border: 0xcdc7ba,
    card: 0xfbf9f4,
    field: 0xfffdf8,
    accent: 0xe9e4d8,
    primary: 0x1f5d8f,
    ring: 0x2f8cc4,
    border: 0x11100f26,
    input_border: 0x11100f40,
    edge_highlight: 0xffffffb3,
    bubble_hi: 0x2a3340,
    bubble: 0x1f262f,
    bubble_edge: 0x38424f,
    bubble_text: 0xeef1f5,
    bubble_meta: 0x9fb0bf,
};

pub const DARK: Palette = Palette {
    dark: true,
    chassis_hi: 0x1d1e22,
    chassis: 0x141518,
    header: 0x0b0c0e,
    header_hi: 0x111215,
    header_edge: 0x232429,
    status_label: 0xa3a5aa,
    empty_pad: 0x2a2b30,
    empty_pad_hi: 0x2e2f34,
    empty_pad_edge: 0x3a3b41,
    pad_number: 0x7c7e84,
    pad_title: 0xe7e7ea,
    add_icon: 0x63a9e0,
    foreground: 0xe6e7ea,
    muted: 0x9a9ca2,
    paper: 0x1c1d21,
    paper_border: 0x2f3036,
    card: 0x24252a,
    field: 0x16171a,
    accent: 0x303137,
    primary: 0x63a9e0,
    ring: 0x5babdb,
    border: 0xffffff1f,
    input_border: 0xffffff2e,
    edge_highlight: 0xffffff12,
    bubble_hi: 0x2f3742,
    bubble: 0x262d36,
    bubble_edge: 0x3b4552,
    bubble_text: 0xeef1f5,
    bubble_meta: 0x9fb0bf,
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);

/// Follow the window's effective macOS appearance. Returns true when it changed.
pub fn set_appearance(appearance: WindowAppearance) -> bool {
    let dark = matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
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

// Appearance-independent brand and pad colors.
pub const LOGO_RED: u32 = 0xc8301c;
pub const LED_GREEN: u32 = 0x4ea865;
pub const LED_AMBER: u32 = 0xe8a33a;
pub const LED_RED: u32 = 0xe5402a;
pub const PAD_FACE: u32 = 0xa4a6aa;
pub const PAD_FACE_HI: u32 = 0xbabcbf;
pub const PAD_GLOW: u32 = 0x6aa8dc;
pub const PAD_FOCUS: u32 = 0xe6dc6a;
pub const PAD_INK: u32 = 0x45484d;

pub const SANS: &str = "Inter";
/// Pad-printed hardware lettering (wordmark, key caps, labels). The bundled Inter is a
/// variable font whose weights GPUI cannot select, and Helvetica Neue ships with every Mac.
pub const PRINT: &str = "Helvetica Neue";

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

/// Physical key colorways.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Neutral key; light grey rubber in light mode, dark rubber in dark mode.
    Grey,
    /// Lit blue key: the primary action.
    Blue,
    /// Red key: destructive confirmation.
    Red,
}
impl Key {
    // (face top, face bottom, hover top, hover bottom, border, skirt, label)
    fn colors(self) -> (u32, u32, u32, u32, u32, u32, u32) {
        match self {
            Self::Grey if p().dark => (
                0x3a3c41, 0x2c2d32, 0x45474d, 0x34363b, 0x0a0a0c, 0x050506, 0xd6dade,
            ),
            Self::Grey => (
                0xe2e4e4, 0xc9cdcd, 0xe8eaea, 0xd2d6d6, 0x9a9fa3, 0x868b8f, 0x1e2228,
            ),
            Self::Blue => (
                0x7ccdfb, 0x56bdf9, 0x8ad3fb, 0x66c4f9, 0x2f8cc4, 0x2474a6, 0x0b2233,
            ),
            Self::Red => (
                0xd2442e, 0xb8321f, 0xd9503a, 0xc23a27, 0x8e2616, 0x6f1d11, 0xfff0ec,
            ),
        }
    }
    pub fn label(self) -> u32 {
        self.colors().6
    }
}

/// Styles any stateful element as a raised key that bottoms out when pressed.
pub fn key<E: Styled + StatefulInteractiveElement>(element: E, kind: Key) -> E {
    let (top, bottom, hover_top, hover_bottom, border, skirt, label) = kind.colors();
    let rest = move || {
        vec![
            shadow(0., 2., 0., 0., (skirt << 8) | 0xff),
            shadow(0., 3., 4., 0., 0x00000040),
            inset(0., 1., 0., 0., 0xffffff40),
            inset(0., -1., 0., 0., 0x00000026),
        ]
    };
    let ring_color = p().ring;
    element
        .relative()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(border))
        .bg(vertical(top, bottom, 1.))
        .shadow(rest())
        .text_color(rgb(label))
        .font_family(PRINT)
        .font_weight(FontWeight::BOLD)
        .text_size(px(9.))
        .cursor_pointer()
        .hover(move |s| s.bg(vertical(hover_top, hover_bottom, 1.)))
        .active(move |s| {
            s.top(px(2.)).shadow(vec![
                shadow(0., 0., 1., 0., 0x00000059),
                inset(0., 2., 3., 0., 0x00000040),
            ])
        })
        .focus_visible(move |s| {
            let mut ring = rest();
            ring.insert(0, shadow(0., 0., 0., 2., (ring_color << 8) | 0xff));
            s.shadow(ring)
        })
}

/// Backlit rubber pad: grey face with a blue rim; keyboard focus lights it yellow.
/// Unlit pads are empty slots recessed into the chassis.
pub fn pad<E: Styled + StatefulInteractiveElement>(element: E, lit: bool) -> E {
    let glow = |color: u32, alpha: u32, blur: f32| {
        vec![
            shadow(0., 0., 0., 2., (color << 8) | alpha),
            shadow(0., 0., blur, 1., (color << 8) | 0x33),
            inset(0., 1., 0., 0., 0xffffff59),
            inset(0., -2., 3., 0., 0x00000026),
        ]
    };
    let element = element.relative().rounded(px(4.)).border_1();
    if !lit {
        let palette = p();
        let rest = vec![
            shadow(0., 1., 1., 0., 0x0000001a),
            inset(0., 1., 0., 0., palette.edge_highlight),
        ];
        return element
            .bg(vertical(palette.empty_pad_hi, palette.empty_pad, 1.))
            .border_color(rgb(palette.empty_pad_edge))
            .shadow(rest.clone())
            .focus_visible(move |s| {
                let mut ring = rest.clone();
                ring.insert(0, shadow(0., 0., 0., 2., (PAD_FOCUS << 8) | 0xff));
                s.shadow(ring)
            });
    }
    element
        .bg(vertical(PAD_FACE_HI, PAD_FACE, 1.))
        .border_color(rgb(0x8a8d92))
        .shadow(glow(PAD_GLOW, 0xd9, 6.))
        .cursor_pointer()
        .hover(move |s| {
            s.bg(vertical(0xc3c5c8, 0xadafb3, 1.))
                .shadow(glow(PAD_GLOW, 0xff, 10.))
        })
        .active(move |s| {
            s.bg(vertical(0x9a9da1, 0xa5a7ab, 1.))
                .border_color(rgb(0x7a7d82))
                .shadow(vec![
                    shadow(0., 0., 0., 2., 0x8ccbf0ff),
                    shadow(0., 0., 12., 3., 0x6aa8dc80),
                    inset(0., 2., 4., 0., 0x00000059),
                ])
        })
        .focus_visible(move |s| s.shadow(glow(PAD_FOCUS, 0xff, 10.)))
}

/// Small status LED pill.
pub fn led(color: u32) -> Div {
    div()
        .w(px(14.))
        .h(px(6.))
        .rounded(px(3.))
        .flex_shrink_0()
        .bg(rgb(color))
        .shadow(vec![
            shadow(0., 0., 5., 0., (color << 8) | 0x80),
            inset(0., 1., 0., 0., 0xffffff40),
        ])
}

/// Printed label: tiny tracked caps.
pub fn silkscreen(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .font_family(PRINT)
        .text_size(px(9.5))
        .line_height(px(12.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(color))
        .whitespace_nowrap()
        .child(spaced(&text.into()))
}
/// GPUI has no letter-spacing, so printed caps get a six-per-em space between letters,
/// approximating the design's tracking (hair spaces shape to almost nothing).
pub fn spaced(text: &str) -> String {
    const TRACKING: &str = "\u{2006}";
    let mut out = String::with_capacity(text.len() * 4);
    for (index, ch) in text.to_uppercase().chars().enumerate() {
        if index > 0 {
            out.push_str(TRACKING);
        }
        out.push(ch);
    }
    out
}

/// The sheet that holds long-form content on the chassis.
pub fn paper() -> Div {
    let palette = p();
    div()
        .rounded(px(6.))
        .bg(rgb(palette.paper))
        .border_1()
        .border_color(rgb(palette.paper_border))
        .shadow(vec![
            shadow(0., 1., 0., 0., palette.edge_highlight),
            inset(0., 1., 3., 0., 0x0000001f),
        ])
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

/// The WIESEL wordmark: Inter Black (OFL, bundled) outlines with tight tracking and a slight
/// horizontal condense, matching the design's lettering. Painted with the element's color.
pub fn wordmark(height: f32) -> Svg {
    svg()
        .path(WORDMARK)
        .w(px(height * WORDMARK_ASPECT))
        .h(px(height))
        .flex_shrink_0()
        .text_color(rgb(LOGO_RED))
}
const WORDMARK: &str = "wordmark";
const WORDMARK_ASPECT: f32 = 6556. / 1490.;
const WORDMARK_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 6556.0 1490"><path fill="white" d="M350.9 1490 0 0H401.6L507.3 578Q527.7 687 545.5 796.5Q563.3 906 580.6 1015Q597.9 1124 613.9 1234H528.6Q547.3 1124 566 1015Q584.6 906 604.6 796.5Q624.6 687 646.8 578L764.1 0H1123L1240.3 578Q1262.5 687 1282.5 796.5Q1302.5 906 1321.2 1015Q1339.8 1124 1358.5 1234H1272.3Q1289.2 1124 1306 1015Q1322.9 906 1341.1 796.5Q1359.4 687 1378.9 578L1485.5 0H1887.1L1536.2 1490H1130.1L980 802Q963.1 722 943.1 606Q923.1 490 889.4 322H997.7Q964 490 944.4 606Q924.9 722 907.1 802L757 1490ZM2247.8 0V1490H1878.2V0ZM2310 1490V0H3335.3V352H2679.6V576H3284.7V916H2679.6V1138H3335.3V1490ZM3934.1 1514Q3751.1 1514 3624.1 1454Q3497 1394 3431.3 1277.5Q3365.5 1161 3365.5 994H3714.7Q3717.3 1054 3745.3 1097Q3773.3 1140 3822.6 1163Q3871.9 1186 3938.6 1186Q3997.2 1186 4040.7 1169.5Q4084.3 1153 4108.7 1123.5Q4133.1 1094 4133.1 1055Q4133.1 1019 4110 992Q4086.9 965 4039 945Q3991 925 3916.4 909L3784.9 881Q3593 840 3491.7 729Q3390.4 618 3390.4 452Q3390.4 310 3458.8 203Q3527.2 96 3648.9 36Q3770.7 -24 3931.5 -24Q4096.7 -24 4215.8 35Q4334.8 94 4400.6 205Q4466.3 316 4470.8 472H4131.4Q4125.2 394 4073.2 349Q4021.2 304 3938.6 304Q3887 304 3847.1 320.5Q3807.1 337 3784.4 365.5Q3761.8 394 3761.8 432Q3761.8 468 3782.2 493Q3802.6 518 3846.6 536.5Q3890.6 555 3959 569L4065.6 591Q4179.4 615 4261.5 652Q4343.7 689 4396.1 742Q4448.6 795 4473.4 865Q4498.3 935 4498.3 1024Q4498.3 1180 4432.1 1289.5Q4365.9 1399 4239.8 1456.5Q4113.6 1514 3934.1 1514ZM4546.3 1490V0H5571.6V352H4915.9V576H5520.9V916H4915.9V1138H5571.6V1490ZM5616 1490V0H5985.6V1138H6556V1490Z"/></svg>"#;

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
    (
        "copy",
        r#"<rect width="14" height="14" x="8" y="8" rx="2"/><path d="M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2"/>"#,
    ),
];
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == WORDMARK {
            return Ok(Some(Cow::Borrowed(WORDMARK_SVG.as_bytes())));
        }
        Ok(ICONS.iter().find(|(name, _)| *name == path).map(|(_, body)| {
            Cow::Owned(format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#).into_bytes())
        }))
    }
    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .chain(std::iter::once(SharedString::from(WORDMARK)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Assets, DARK, LIGHT, icon, p, set_appearance};
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
        assert_ne!(LIGHT.chassis, DARK.chassis);
        assert_ne!(LIGHT.foreground, DARK.foreground);
        // Only assert the light default: other tests in this process render in Light.
        set_appearance(WindowAppearance::Light);
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
