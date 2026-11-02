//! Catppuccin Mocha, assigned to roles per the design's role map.
use ratatui::style::Color;

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const CRUST: Color = rgb(0x11111b);
pub const MANTLE: Color = rgb(0x181825);
pub const BASE: Color = rgb(0x1e1e2e);
pub const SURFACE0: Color = rgb(0x313244);
pub const SURFACE1: Color = rgb(0x45475a);
pub const SURFACE2: Color = rgb(0x585b70);
pub const TEXT: Color = rgb(0xcdd6f4);
pub const SUBTEXT1: Color = rgb(0xbac2de);
pub const SUBTEXT0: Color = rgb(0xa6adc8);
pub const OVERLAY2: Color = rgb(0x9399b2);
pub const MAUVE: Color = rgb(0xcba6f7);
pub const LAVENDER: Color = rgb(0xb4befe);
pub const PEACH: Color = rgb(0xfab387);
pub const GREEN: Color = rgb(0xa6e3a1);
pub const SKY: Color = rgb(0x89dceb);
pub const BLUE: Color = rgb(0x89b4fa);
pub const TEAL: Color = rgb(0x94e2d5);
pub const YELLOW: Color = rgb(0xf9e2af);
pub const RED: Color = rgb(0xf38ba8);
pub const ROSEWATER: Color = rgb(0xf5e0dc);
pub const PINK: Color = rgb(0xf5c2e7);
pub const SAPPHIRE: Color = rgb(0x74c7ec);
pub const MAROON: Color = rgb(0xeba0ac);
pub const FLAMINGO: Color = rgb(0xf2cdcd);

pub const ACCENTS: [&str; 4] = ["mauve", "blue", "pink", "green"];

pub fn accent(name: &str) -> Color {
    match name {
        "blue" => BLUE,
        "pink" => PINK,
        "green" => GREEN,
        _ => MAUVE,
    }
}

/// Stable colour for an item without a poster, matching the design's halftone placeholders.
pub fn tint(seed: &str) -> Color {
    const TINTS: [Color; 10] = [RED, SAPPHIRE, YELLOW, PEACH, LAVENDER, MAROON, FLAMINGO, PINK, GREEN, SKY];
    let sum = seed.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    TINTS[sum % TINTS.len()]
}

pub fn quality_color(quality: &str) -> Color {
    match quality {
        "2160p" | "1440p" | "1080p" => GREEN,
        "720p" => SKY,
        _ => SUBTEXT1,
    }
}

pub fn resource_color(resource: &str) -> Color {
    match resource {
        "catalog" => BLUE,
        "meta" => TEAL,
        "stream" => GREEN,
        "subtitles" => PEACH,
        _ => SUBTEXT1,
    }
}
