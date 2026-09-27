//! Minimal ANSI SGR (color/style) to ratatui text conversion, enough for `nvd --color always`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

fn basic(n: u8, bright: bool) -> Color {
    match (n, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (7, false) => Color::Gray,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        _ => Color::White,
    }
}

/// Reads an extended color (`5;n` or `2;r;g;b`) after a 38/48 code.
fn extended(codes: &mut impl Iterator<Item = u8>) -> Option<Color> {
    match codes.next()? {
        5 => Some(Color::Indexed(codes.next()?)),
        2 => Some(Color::Rgb(codes.next()?, codes.next()?, codes.next()?)),
        _ => None,
    }
}

fn apply_sgr(mut style: Style, params: &str) -> Style {
    let codes: Vec<u8> = if params.is_empty() {
        vec![0]
    } else {
        params.split(';').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let mut codes = codes.into_iter();
    while let Some(code) = codes.next() {
        style = match code {
            0 => Style::new(),
            1 => style.add_modifier(Modifier::BOLD),
            2 => style.add_modifier(Modifier::DIM),
            3 => style.add_modifier(Modifier::ITALIC),
            4 => style.add_modifier(Modifier::UNDERLINED),
            7 => style.add_modifier(Modifier::REVERSED),
            22 => style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style.remove_modifier(Modifier::ITALIC),
            24 => style.remove_modifier(Modifier::UNDERLINED),
            27 => style.remove_modifier(Modifier::REVERSED),
            30..=37 => style.fg(basic(code - 30, false)),
            90..=97 => style.fg(basic(code - 90, true)),
            39 => style.fg(Color::Reset),
            40..=47 => style.bg(basic(code - 40, false)),
            100..=107 => style.bg(basic(code - 100, true)),
            49 => style.bg(Color::Reset),
            38 => extended(&mut codes).map_or(style, |c| style.fg(c)),
            48 => extended(&mut codes).map_or(style, |c| style.bg(c)),
            _ => style,
        };
    }
    style
}

/// Converts text with ANSI SGR sequences to styled lines; other escape sequences are dropped.
pub fn to_text(input: &str) -> Text<'static> {
    let mut style = Style::new();
    let lines: Vec<Line> = input
        .lines()
        .map(|raw| {
            let mut spans = Vec::new();
            let mut rest = raw;
            while let Some(pos) = rest.find('\x1b') {
                if pos > 0 {
                    spans.push(Span::styled(rest[..pos].to_owned(), style));
                }
                rest = &rest[pos + 1..];
                if let Some(csi) = rest.strip_prefix('[') {
                    let end = csi
                        .find(|c: char| c.is_ascii_alphabetic())
                        .unwrap_or(csi.len());
                    if csi[end..].starts_with('m') {
                        style = apply_sgr(style, &csi[..end]);
                    }
                    rest = csi.get(end + 1..).unwrap_or("");
                }
            }
            if !rest.is_empty() {
                spans.push(Span::styled(rest.to_owned(), style));
            }
            Line::from(spans)
        })
        .collect();
    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_sgr_sequences() {
        let text =
            to_text("[\x1b[1mU\x1b[0m\x1b[1m\x1b[32;1m*\x1b[0m]  foo \x1b[33m1.0\x1b[0m\nplain");
        assert_eq!(text.lines.len(), 2);
        let spans = &text.lines[0].spans;
        let content: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(content, "[U*]  foo 1.0");
        assert_eq!(spans[1].content, "U");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        // Back-to-back sequences with no text between them produce no spans.
        assert_eq!(spans[2].content, "*");
        assert_eq!(spans[2].style.fg, Some(Color::Green));
        assert_eq!(spans[4].content, "1.0");
        assert_eq!(spans[4].style.fg, Some(Color::Yellow));
        assert_eq!(text.lines[1].spans[0].content, "plain");
    }

    #[test]
    fn extended_colors_and_unknown_sequences() {
        let text = to_text("\x1b[38;5;208ma\x1b[48;2;1;2;3mb\x1b[2Kc");
        let spans = &text.lines[0].spans;
        assert_eq!(spans[0].style.fg, Some(Color::Indexed(208)));
        assert_eq!(spans[1].style.bg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(spans[2].content, "c");
    }
}
