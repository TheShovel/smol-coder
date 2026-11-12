use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new() -> Self {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .remove("base16-ocean.dark")
            .or_else(|| themes.themes.values().next().cloned())
            .unwrap_or_default();
        Self { syntaxes, theme }
    }

    pub fn highlight(&self, path: &Path, lines: &[String]) -> Vec<Vec<(Style, String)>> {
        let Some(syntax) = self.syntax_for(path, lines) else {
            return plain(lines);
        };

        let text = lines.join("\n");
        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::with_capacity(lines.len());

        for line in LinesWithEndings::from(&text) {
            let mut spans = Vec::new();
            if let Ok(ranges) = highlighter.highlight_line(line, &self.syntaxes) {
                for (style, text) in ranges {
                    let text = text.trim_end_matches(['\n', '\r']);
                    if text.is_empty() {
                        continue;
                    }
                    spans.push((map_style(style), text.to_string()));
                }
            }
            out.push(spans);
        }

        out.resize(lines.len(), Vec::new());
        out.truncate(lines.len());
        out
    }

    fn syntax_for<'a>(&'a self, path: &Path, lines: &[String]) -> Option<&'a SyntaxReference> {
        let extension = path.extension().and_then(|ext| ext.to_str());
        if let Some(syntax) = extension.and_then(|ext| self.syntaxes.find_syntax_by_extension(ext))
        {
            return Some(syntax);
        }

        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            if let Some(syntax) = self.syntaxes.find_syntax_by_token(name) {
                return Some(syntax);
            }
        }

        if let Some(first) = lines.first() {
            if first.starts_with("#!") {
                if let Some(syntax) = self.syntaxes.find_syntax_by_first_line(first) {
                    return Some(syntax);
                }
            }
        }

        None
    }
}

fn map_style(style: syntect::highlighting::Style) -> Style {
    let mut out = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

fn plain(lines: &[String]) -> Vec<Vec<(Style, String)>> {
    lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                Vec::new()
            } else {
                vec![(Style::default().fg(theme::FG), line.clone())]
            }
        })
        .collect()
}
