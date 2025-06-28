use anyhow::{Context, Result};
use ratatui::text::{Line, Span, Text};
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    io::Cursor,
    path::Path,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{self, Color, FontStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
};

use crate::github::{code::SearchItem, TextMatch};

const ANSI_THEME: &[u8] = include_bytes!("ansi.tmTheme");

pub type Fragments = Vec<Text<'static>>;

pub struct MarkdownHighlighter {
    syntax: SyntaxSet,
    theme: Theme,
}

impl Default for MarkdownHighlighter {
    fn default() -> Self {
        let mut theme_cursor = Cursor::new(ANSI_THEME);
        Self {
            syntax: SyntaxSet::load_defaults_newlines(),
            theme: ThemeSet::load_from_reader(&mut theme_cursor).expect("Loading theme"),
        }
    }
}

impl MarkdownHighlighter {
    pub fn highlight(&self, text: &str) -> Result<Text> {
        let syntax = self
            .syntax
            .find_syntax_by_extension("md")
            .context("markdown syntax not found")?;
        let mut h = HighlightLines::new(syntax, &self.theme);

        let mut highlighted_lines = Vec::new();
        for line in text.lines() {
            let highlights = h.highlight_line(line, &self.syntax)?;
            highlighted_lines.push(to_line_widget(highlights));
        }

        Ok(Text::from(highlighted_lines))
    }
}

pub struct PreviewCache {
    cache: HashMap<String, Fragments>, // url->content
    syntax: SyntaxSet,
    theme: Theme,
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewCache {
    pub fn new() -> Self {
        let mut theme_cursor = Cursor::new(ANSI_THEME);
        Self {
            cache: HashMap::new(),
            syntax: SyntaxSet::load_defaults_newlines(),
            theme: ThemeSet::load_from_reader(&mut theme_cursor).expect("Loading theme"),
        }
    }

    pub fn contains(&self, url: &str) -> bool {
        self.cache.contains_key(url)
    }

    pub fn get(&self, url: &str) -> Option<&Fragments> {
        self.cache.get(url)
    }

    pub fn insert_placeholder(&mut self, url: impl Into<String> + Display) {
        self.cache.insert(url.into(), vec![]);
    }

    pub fn insert(&mut self, item: SearchItem, content: &str) -> Result<()> {
        tracing::debug!("Caching content for: {}", item.url);
        let syntax = Path::new(&item.path)
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| self.syntax.find_syntax_by_extension(ext))
            .or_else(|| {
                content
                    .lines()
                    .next()
                    .and_then(|line| self.syntax.find_syntax_by_first_line(line))
            })
            .unwrap_or_else(|| self.syntax.find_syntax_plain_text());
        let mut h = HighlightLines::new(syntax, &self.theme);

        let mut matching_lines = Vec::new();
        let mut highlighted_lines = Vec::new();
        let fragments = matching_strings(&item.text_matches);
        tracing::trace!("Finding fragments matching: {fragments:?}");

        for (i, line) in content.lines().enumerate() {
            let mut highlights = h.highlight_line(line, &self.syntax)?;
            if let Some(frag) = fragments.iter().find(|&frag| line.contains(frag)) {
                tracing::trace!("Matched '{frag}' on line {i}");
                matching_lines.push(i);
                for (style, s) in highlights.iter_mut() {
                    if !s.contains(frag) {
                        continue;
                    }
                    // Use the ANSI red slot
                    style.foreground = Color {
                        r: 1,
                        g: 0,
                        b: 0,
                        a: 0,
                    };
                    style.font_style = FontStyle::BOLD;
                }
            }
            highlighted_lines.push(highlights);
        }

        let spans = line_spans(matching_lines, highlighted_lines.len() - 1);
        if spans.is_empty() {
            tracing::error!("No matches found: {}", item.url);
        }

        self.cache.insert(
            item.url,
            spans
                .into_iter()
                .map(|range| {
                    Text::from_iter(range.map(|n| to_line_widget(highlighted_lines[n].clone())))
                })
                .collect(),
        );
        Ok(())
    }
}

// Github doesn't tell us where in the document a fragment matched.
// Instead, we have to pick out each matching fragment and try to find it ourselves.
fn matching_strings(matches: &Vec<TextMatch>) -> HashSet<&str> {
    let mut set = HashSet::new();
    for m in matches {
        for m in &m.matches {
            set.insert(m.text.as_str());
        }
    }
    set
}

// Given a list of matching line numbers, return a list of start/end pairs that encompass matching lines with context
fn line_spans(line_numbers: Vec<usize>, max_line: usize) -> Vec<std::ops::RangeInclusive<usize>> {
    // TODO: Make configurable
    // TODO: Merge nearby segments
    const CONTEXT_LINES: usize = 5;

    let mut spans = Vec::new();
    for n in line_numbers {
        let range = n.saturating_sub(CONTEXT_LINES)..=max_line.min(n + CONTEXT_LINES);
        tracing::trace!("Including preview range '{range:?}'");
        spans.push(range);
    }
    spans
}

#[test]
fn test_line_spans() {
    assert_eq!(
        line_spans(vec![1, 5, 8, 20, 24], 28),
        vec![0..=6, 0..=10, 3..=13, 15..=25, 19..=28]
    );
}

// Borrowed from https://github.com/sxyazi/yazi/pull/460/files
fn to_ansi_color(color: highlighting::Color) -> Option<ratatui::style::Color> {
    if color.a == 0 {
        // Themes can specify one of the user-configurable terminal colors by
        // encoding them as #RRGGBBAA with AA set to 00 (transparent) and RR set
        // to the 8-bit color palette number. The built-in themes ansi, base16,
        // and base16-256 use this.
        Some(match color.r {
            // For the first 8 colors, use the Color enum to produce ANSI escape
            // sequences using codes 30-37 (foreground) and 40-47 (background).
            // For example, red foreground is \x1b[31m. This works on terminals
            // without 256-color support.
            0x00 => ratatui::style::Color::Black,
            0x01 => ratatui::style::Color::Red,
            0x02 => ratatui::style::Color::Green,
            0x03 => ratatui::style::Color::Yellow,
            0x04 => ratatui::style::Color::Blue,
            0x05 => ratatui::style::Color::Magenta,
            0x06 => ratatui::style::Color::Cyan,
            0x07 => ratatui::style::Color::White,
            // For all other colors, use Fixed to produce escape sequences using
            // codes 38;5 (foreground) and 48;5 (background). For example,
            // bright red foreground is \x1b[38;5;9m. This only works on
            // terminals with 256-color support.
            //
            // TODO: When ansi_term adds support for bright variants using codes
            // 90-97 (foreground) and 100-107 (background), we should use those
            // for values 0x08 to 0x0f and only use Fixed for 0x10 to 0xff.
            n => ratatui::style::Color::Indexed(n),
        })
    } else if color.a == 1 {
        // Themes can specify the terminal's default foreground/background color
        // (i.e. no escape sequence) using the encoding #RRGGBBAA with AA set to
        // 01. The built-in theme ansi uses this.
        None
    } else {
        Some(ratatui::style::Color::Rgb(color.r, color.g, color.b))
    }
}

// Convert syntect highlighting to ANSI terminal colors
// See https://github.com/trishume/syntect/issues/309
// Borrowed from https://github.com/sxyazi/yazi/pull/460/files
fn to_line_widget(regions: Vec<(highlighting::Style, &str)>) -> Line<'static> {
    let mut line = Line::default();
    for (style, s) in regions {
        let mut modifier = ratatui::style::Modifier::empty();
        if style.font_style.contains(highlighting::FontStyle::BOLD) {
            modifier |= ratatui::style::Modifier::BOLD;
        }
        if style.font_style.contains(highlighting::FontStyle::ITALIC) {
            modifier |= ratatui::style::Modifier::ITALIC;
        }
        if style
            .font_style
            .contains(highlighting::FontStyle::UNDERLINE)
        {
            modifier |= ratatui::style::Modifier::UNDERLINED;
        }

        line.push_span(Span {
            content: s.to_string().into(),
            style: ratatui::style::Style {
                fg: to_ansi_color(style.foreground),
                // bg: Self::to_ansi_color(style.background),
                add_modifier: modifier,
                ..Default::default()
            },
        })
    }

    line
}
