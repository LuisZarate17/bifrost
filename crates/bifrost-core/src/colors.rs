//! The suite's colors, read from the one file that holds them:
//! `packages/ui/src/colors.json`.
//!
//! Vidar's popup, Mimir's window and Hugin's terminal each wear a color of their
//! own. The file lives under `packages/ui` so the web side imports it as
//! `@bifrost/ui/colors.json`; this module embeds the same bytes at compile time,
//! so the Rust side can never drift from what the apps draw. Hugin (Stage I, I2)
//! will use `palette().hugin` for its prompt, approvals and results.
//!
//! Each app has two values, one for a dark ground and one for a light one. The
//! file is embedded and checked by the tests below, so [`palette`] only panics if
//! someone breaks the file's shape, and `cargo test` says so first.

use serde::Deserialize;
use std::sync::OnceLock;

/// One app's color: the name it goes by, and its value on each ground.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppColor {
    /// "Wolf's Bane", "Deep Well", "Raven Violet".
    pub name: String,
    /// `#rrggbb`, for a dark ground.
    pub dark: String,
    /// `#rrggbb`, for a light ground.
    pub light: String,
}

/// Every color the suite shares.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Palette {
    pub vidar: AppColor,
    pub mimir: AppColor,
    pub hugin: AppColor,
    /// `#rrggbb`: the suite's near-black, the badges' ink.
    pub ink: String,
    /// `#rrggbb`: the suite's off-white.
    pub ivory: String,
}

const SOURCE: &str = include_str!("../../../packages/ui/src/colors.json");

/// The palette, parsed on first use and kept.
pub fn palette() -> &'static Palette {
    static PALETTE: OnceLock<Palette> = OnceLock::new();
    PALETTE.get_or_init(|| {
        serde_json::from_str(SOURCE).expect("packages/ui/src/colors.json no longer matches Palette")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_hex_color(s: &str) -> bool {
        s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
    }

    #[test]
    fn the_shared_file_parses() {
        let parsed = serde_json::from_str::<Palette>(SOURCE);
        assert!(parsed.is_ok(), "{:?}", parsed.err());
        assert_eq!(&parsed.unwrap(), palette());
    }

    #[test]
    fn every_color_is_rrggbb() {
        let p = palette();
        for (what, value) in [
            ("vidar.dark", &p.vidar.dark),
            ("vidar.light", &p.vidar.light),
            ("mimir.dark", &p.mimir.dark),
            ("mimir.light", &p.mimir.light),
            ("hugin.dark", &p.hugin.dark),
            ("hugin.light", &p.hugin.light),
            ("ink", &p.ink),
            ("ivory", &p.ivory),
        ] {
            assert!(is_hex_color(value), "{what} is {value:?}, not #rrggbb");
        }
    }

    #[test]
    fn raven_violet_is_hugins() {
        let hugin = &palette().hugin;
        assert_eq!(hugin.name, "Raven Violet");
        assert_eq!(hugin.dark, "#8b7cf8");
    }
}
