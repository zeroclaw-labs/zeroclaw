//! Terminal colour-depth detection and RGB down-conversion.
//!
//! ratatui emits `Color::Rgb` as 24-bit `\e[38;2;…m` SGR sequences
//! unconditionally. Terminals that cap at 256 colours (older macOS
//! Terminal.app, and tmux/screen advertising `screen-256color`) either
//! ignore or mangle those sequences, producing the washed-out / garbled
//! palette users hit over SSH+tmux. Detecting the supported depth once at
//! startup and snapping every themed `Rgb` to the nearest xterm-256 (or
//! ANSI-16) index keeps the palette legible everywhere instead of forcing
//! the user to fall back to the uncoloured `terminal` theme.

use std::sync::OnceLock;

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorDepth {
    /// 16 ANSI colours only (very old terminals, `TERM=…-16color`, `dumb`).
    Ansi16,
    /// xterm-256 palette.
    Ansi256,
    /// 24-bit RGB.
    TrueColor,
}

static DEPTH: OnceLock<ColorDepth> = OnceLock::new();

/// The terminal's colour depth, detected once from the environment on first
/// use and memoised. Detection reads `ZEROCODE_COLOR`, `COLORTERM`, `TERM`,
/// `TERM_PROGRAM`, and tmux presence. Lazy initialisation means callers
/// never need an explicit startup hook — the first themed colour to render
/// triggers it.
pub(crate) fn active() -> ColorDepth {
    *DEPTH.get_or_init(|| {
        detect_from_env(
            std::env::var("ZEROCODE_COLOR").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
            std::env::var("TMUX").is_ok(),
        )
    })
}

/// Pure detection core, split out for testing. `override_var` is the value
/// of `ZEROCODE_COLOR` (`truecolor`/`24bit`, `256`, `16`/`ansi`), which
/// short-circuits auto-detection when set to a recognised value.
fn detect_from_env(
    override_var: Option<&str>,
    colorterm: Option<&str>,
    term: Option<&str>,
    term_program: Option<&str>,
    in_tmux: bool,
) -> ColorDepth {
    if let Some(forced) = override_var.and_then(parse_override) {
        return forced;
    }

    let term = term.unwrap_or("");

    if term.is_empty() || term == "dumb" {
        return ColorDepth::Ansi16;
    }

    // tmux/screen multiplex truecolor only when explicitly configured; their
    // default `*-256color` TERM clamps 24-bit SGR. Treat them as 256 unless
    // COLORTERM is propagated through, which only happens with passthrough on.
    let multiplexed = in_tmux || term.starts_with("screen") || term.starts_with("tmux");

    let colorterm_truecolor = matches!(colorterm, Some(c) if c.eq_ignore_ascii_case("truecolor") || c.eq_ignore_ascii_case("24bit"));

    if colorterm_truecolor && !multiplexed {
        return ColorDepth::TrueColor;
    }

    // Terminals known to do truecolor even without COLORTERM, when not behind
    // a multiplexer that would clamp it.
    let term_program_truecolor = matches!(
        term_program,
        Some(p) if p.eq_ignore_ascii_case("iTerm.app")
            || p.eq_ignore_ascii_case("WezTerm")
            || p.eq_ignore_ascii_case("vscode")
            || p.eq_ignore_ascii_case("ghostty")
    );
    if term_program_truecolor && !multiplexed {
        return ColorDepth::TrueColor;
    }

    if term.contains("truecolor") || term.contains("direct") {
        return ColorDepth::TrueColor;
    }

    if term.contains("256") {
        return ColorDepth::Ansi256;
    }

    if term.ends_with("-16color") {
        return ColorDepth::Ansi16;
    }

    // Conservative default: most modern xterm-family TERMs support 256.
    ColorDepth::Ansi256
}

fn parse_override(v: &str) -> Option<ColorDepth> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("truecolor") || v.eq_ignore_ascii_case("24bit") || v == "24" {
        Some(ColorDepth::TrueColor)
    } else if v == "256" {
        Some(ColorDepth::Ansi256)
    } else if v == "16" || v.eq_ignore_ascii_case("ansi") {
        Some(ColorDepth::Ansi16)
    } else {
        None
    }
}

/// Snap a colour to what the active terminal can render. `Rgb` is passed
/// through untouched on truecolor terminals, down-converted to the nearest
/// xterm-256 index on 256-colour terminals, and to the nearest ANSI-16
/// colour otherwise. Non-`Rgb` colours (named, indexed, `Reset`) are
/// already renderable everywhere and pass through.
pub(crate) fn downgrade(color: Color) -> Color {
    downgrade_at(color, active())
}

fn downgrade_at(color: Color, depth: ColorDepth) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    match depth {
        ColorDepth::TrueColor => color,
        ColorDepth::Ansi256 => Color::Indexed(rgb_to_xterm256(r, g, b)),
        ColorDepth::Ansi16 => rgb_to_ansi16(r, g, b),
    }
}

/// Nearest xterm-256 index for an RGB triple. Considers both the 6×6×6
/// colour cube (indices 16–231) and the 24-step grey ramp (232–255), and
/// returns whichever is closer, so neutral theme colours map to crisp greys
/// rather than muddy cube entries.
fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    let cube_idx = |v: u8| -> u8 {
        // xterm cube steps: 0, 95, 135, 175, 215, 255.
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as u16 - 35) / 40) as u8
        }
    };
    let cube_val = |i: u8| -> u8 {
        if i == 0 {
            0
        } else {
            (55 + i as u16 * 40) as u8
        }
    };

    let ri = cube_idx(r);
    let gi = cube_idx(g);
    let bi = cube_idx(b);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let cube_dist = dist(r, g, b, cube_val(ri), cube_val(gi), cube_val(bi));

    // Grey ramp: 232 + n maps to 8 + n*10 for n in 0..24.
    let grey_avg = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    let grey_n = if grey_avg < 8 {
        0
    } else if grey_avg > 238 {
        23
    } else {
        ((grey_avg as u16 - 8) / 10) as u8
    };
    let grey_val = (8 + grey_n as u16 * 10) as u8;
    let grey_dist = dist(r, g, b, grey_val, grey_val, grey_val);

    if grey_dist < cube_dist {
        232 + grey_n
    } else {
        cube_index
    }
}

/// Nearest of the 16 ANSI colours, returned as a ratatui named `Color`.
fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Color {
    const PALETTE: [(u8, u8, u8, Color); 16] = [
        (0, 0, 0, Color::Black),
        (128, 0, 0, Color::Red),
        (0, 128, 0, Color::Green),
        (128, 128, 0, Color::Yellow),
        (0, 0, 128, Color::Blue),
        (128, 0, 128, Color::Magenta),
        (0, 128, 128, Color::Cyan),
        (192, 192, 192, Color::Gray),
        (128, 128, 128, Color::DarkGray),
        (255, 0, 0, Color::LightRed),
        (0, 255, 0, Color::LightGreen),
        (255, 255, 0, Color::LightYellow),
        (0, 0, 255, Color::LightBlue),
        (255, 0, 255, Color::LightMagenta),
        (0, 255, 255, Color::LightCyan),
        (255, 255, 255, Color::White),
    ];
    PALETTE
        .iter()
        .min_by_key(|(pr, pg, pb, _)| dist(r, g, b, *pr, *pg, *pb))
        .map(|(_, _, _, c)| *c)
        .unwrap_or(Color::Reset)
}

fn dist(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = r1 as i32 - r2 as i32;
    let dg = g1 as i32 - g2 as i32;
    let db = b1 as i32 - b2 as i32;
    (dr * dr + dg * dg + db * db) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_forces_depth() {
        assert_eq!(
            detect_from_env(
                Some("16"),
                Some("truecolor"),
                Some("xterm-256color"),
                None,
                false
            ),
            ColorDepth::Ansi16
        );
        assert_eq!(
            detect_from_env(Some("truecolor"), None, Some("dumb"), None, false),
            ColorDepth::TrueColor
        );
        assert_eq!(
            detect_from_env(Some("256"), Some("truecolor"), Some("xterm"), None, false),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn colorterm_truecolor_outside_tmux() {
        assert_eq!(
            detect_from_env(None, Some("truecolor"), Some("xterm-256color"), None, false),
            ColorDepth::TrueColor
        );
        assert_eq!(
            detect_from_env(None, Some("24bit"), Some("xterm-256color"), None, false),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn tmux_clamps_truecolor_to_256() {
        // COLORTERM=truecolor but inside tmux: the multiplexer clamps, so 256.
        assert_eq!(
            detect_from_env(None, Some("truecolor"), Some("screen-256color"), None, true),
            ColorDepth::Ansi256
        );
        assert_eq!(
            detect_from_env(None, Some("truecolor"), Some("tmux-256color"), None, true),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn screen_term_is_256_even_without_tmux_envvar() {
        assert_eq!(
            detect_from_env(
                None,
                Some("truecolor"),
                Some("screen-256color"),
                None,
                false
            ),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn legacy_macos_terminal_app_is_256() {
        // Terminal.app: TERM_PROGRAM=Apple_Terminal, no COLORTERM, 256color.
        assert_eq!(
            detect_from_env(
                None,
                None,
                Some("xterm-256color"),
                Some("Apple_Terminal"),
                false
            ),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn iterm_truecolor_when_not_multiplexed() {
        assert_eq!(
            detect_from_env(None, None, Some("xterm-256color"), Some("iTerm.app"), false),
            ColorDepth::TrueColor
        );
        // …but clamped under tmux.
        assert_eq!(
            detect_from_env(None, None, Some("screen-256color"), Some("iTerm.app"), true),
            ColorDepth::Ansi256
        );
    }

    #[test]
    fn dumb_and_empty_term_are_ansi16() {
        assert_eq!(
            detect_from_env(None, None, Some("dumb"), None, false),
            ColorDepth::Ansi16
        );
        assert_eq!(
            detect_from_env(None, None, Some(""), None, false),
            ColorDepth::Ansi16
        );
        assert_eq!(
            detect_from_env(None, None, None, None, false),
            ColorDepth::Ansi16
        );
    }

    #[test]
    fn sixteen_color_term_suffix() {
        assert_eq!(
            detect_from_env(None, None, Some("xterm-16color"), None, false),
            ColorDepth::Ansi16
        );
    }

    #[test]
    fn direct_term_is_truecolor() {
        assert_eq!(
            detect_from_env(None, None, Some("xterm-direct"), None, false),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn downgrade_passes_non_rgb_through() {
        for d in [
            ColorDepth::Ansi16,
            ColorDepth::Ansi256,
            ColorDepth::TrueColor,
        ] {
            assert_eq!(downgrade_at(Color::Reset, d), Color::Reset);
            assert_eq!(downgrade_at(Color::Red, d), Color::Red);
            assert_eq!(downgrade_at(Color::Indexed(42), d), Color::Indexed(42));
        }
    }

    #[test]
    fn downgrade_truecolor_keeps_rgb() {
        assert_eq!(
            downgrade_at(Color::Rgb(100, 200, 255), ColorDepth::TrueColor),
            Color::Rgb(100, 200, 255)
        );
    }

    #[test]
    fn downgrade_256_indexes_rgb() {
        let white = downgrade_at(Color::Rgb(255, 255, 255), ColorDepth::Ansi256);
        assert!(matches!(white, Color::Indexed(_)));
        let black = downgrade_at(Color::Rgb(0, 0, 0), ColorDepth::Ansi256);
        assert!(matches!(black, Color::Indexed(_)));
    }

    #[test]
    fn downgrade_16_picks_named() {
        assert_eq!(
            downgrade_at(Color::Rgb(255, 0, 0), ColorDepth::Ansi16),
            Color::LightRed
        );
        assert_eq!(
            downgrade_at(Color::Rgb(0, 0, 0), ColorDepth::Ansi16),
            Color::Black
        );
        assert_eq!(
            downgrade_at(Color::Rgb(255, 255, 255), ColorDepth::Ansi16),
            Color::White
        );
    }

    #[test]
    fn grey_maps_to_grey_ramp_not_cube() {
        // A mid grey should land on the grey ramp (232..=255), not a cube
        // entry, so neutral theme text stays crisp.
        let idx = rgb_to_xterm256(128, 128, 128);
        assert!((232..=255).contains(&idx), "expected grey ramp, got {idx}");
    }
}
