// GENERATED from locales.toml by `cargo generate installers` - do not edit by hand.
//
// `locales.toml` at the repo root stays the single source of truth for locale
// codes and labels. Regenerate with `cargo generate installers runtime-locales`;
// CI fails on drift via `cargo generate installers --check`.

/// One selectable locale: its `code` (e.g. `ja`) and display `label`
/// (e.g. 日本語).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleOption {
    pub code: &'static str,
    pub label: &'static str,
}

/// Locales this build knows about, in `locales.toml` order. The first entry is
/// the primary locale.
pub const AVAILABLE_LOCALES: &[LocaleOption] = &[
    LocaleOption {
        code: "en",
        label: "English",
    },
    LocaleOption {
        code: "fr",
        label: "Français",
    },
    LocaleOption {
        code: "ja",
        label: "日本語",
    },
    LocaleOption {
        code: "es",
        label: "Español",
    },
    LocaleOption {
        code: "zh-CN",
        label: "中文",
    },
];
