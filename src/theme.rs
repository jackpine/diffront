use gpui::{Global, Hsla, WindowAppearance, rgb, rgba};

pub const MONO_FONT: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

/// Colors for the current system appearance.
#[derive(Clone, Copy)]
pub struct Theme {
    pub background: Hsla,
    pub panel: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub on_accent: Hsla,
    pub added: Hsla,
    pub removed: Hsla,
    pub added_bg: Hsla,
    pub removed_bg: Hsla,
    pub hunk_bg: Hsla,
    pub comment_bg: Hsla,
    pub text_selection: Hsla,
}

impl Global for Theme {}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    fn dark() -> Self {
        Self {
            background: rgb(0x16181d).into(),
            panel: rgb(0x1e2128).into(),
            hover: rgba(0xffffff12).into(),
            selected: rgba(0x4c8dff33).into(),
            border: rgb(0x2e323b).into(),
            text: rgb(0xdfe2e8).into(),
            muted: rgb(0x868c98).into(),
            accent: rgb(0x4c8dff).into(),
            on_accent: rgb(0xffffff).into(),
            added: rgb(0x57c278).into(),
            removed: rgb(0xef6f6c).into(),
            added_bg: rgba(0x2ea04326).into(),
            removed_bg: rgba(0xf8514926).into(),
            hunk_bg: rgba(0x4c8dff1a).into(),
            comment_bg: rgb(0x242832).into(),
            text_selection: rgba(0x4c8dff55).into(),
        }
    }

    fn light() -> Self {
        Self {
            background: rgb(0xffffff).into(),
            panel: rgb(0xf5f6f8).into(),
            hover: rgba(0x0000000d).into(),
            selected: rgba(0x2563eb1f).into(),
            border: rgb(0xdcdfe5).into(),
            text: rgb(0x1f2329).into(),
            muted: rgb(0x68707d).into(),
            accent: rgb(0x2563eb).into(),
            on_accent: rgb(0xffffff).into(),
            added: rgb(0x1a7f37).into(),
            removed: rgb(0xcf222e).into(),
            added_bg: rgba(0x2ea04322).into(),
            removed_bg: rgba(0xf8514922).into(),
            hunk_bg: rgba(0x2563eb14).into(),
            comment_bg: rgb(0xf5f6f8).into(),
            text_selection: rgba(0x2563eb40).into(),
        }
    }
}
