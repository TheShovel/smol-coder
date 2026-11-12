use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::theme;

pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let normalized = text.replace('\t', "    ");
    let source: Vec<&str> = normalized.split('\n').collect();
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut in_code = false;
    let mut index = 0;

    while index < source.len() {
        let raw = source[index];
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            if !in_code
                && lines
                    .last()
                    .map(|line| !line.spans.is_empty())
                    .unwrap_or(false)
            {
                lines.push(Line::from(""));
            }
            index += 1;
            continue;
        }

        if in_code {
            lines.extend(code_line(raw, width));
            index += 1;
            continue;
        }

        if raw.trim().is_empty() {
            if lines
                .last()
                .map(|line| !line.spans.is_empty())
                .unwrap_or(false)
            {
                lines.push(Line::from(""));
            }
            index += 1;
            continue;
        }

        if let Some((table, consumed)) = table_block(&source, index, width) {
            lines.extend(table);
            index += consumed;
            continue;
        }

        if let Some(heading) = heading(raw) {
            let style = Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD);
            lines.extend(wrap(inline(heading, style), width));
            index += 1;
            continue;
        }

        let indent = raw.len() - trimmed.len();
        let prefix = " ".repeat(indent);

        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let (marker, rest) = task_marker(rest);
            let prefix = format!("{prefix}{marker}");
            let hang = text_width(&prefix);
            let mut spans = vec![Span::styled(prefix, Style::default().fg(theme::DIM))];
            spans.extend(inline(rest, Style::default().fg(theme::FG)));
            lines.extend(wrap_hanging(
                spans,
                width,
                hang,
                Style::default().fg(theme::FAINT),
            ));
            index += 1;
            continue;
        }

        if let Some((marker, rest)) = ordered_item(trimmed) {
            let prefix = format!("{prefix}{marker}");
            let hang = text_width(&prefix);
            let mut spans = vec![Span::styled(prefix, Style::default().fg(theme::DIM))];
            spans.extend(inline(rest, Style::default().fg(theme::FG)));
            lines.extend(wrap_hanging(
                spans,
                width,
                hang,
                Style::default().fg(theme::FAINT),
            ));
            index += 1;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("> ") {
            let prefix = format!("{prefix}| ");
            let hang = text_width(&prefix);
            let mut spans = vec![Span::styled(prefix, Style::default().fg(theme::DIM))];
            spans.extend(inline(
                rest,
                Style::default()
                    .fg(theme::DIM)
                    .add_modifier(Modifier::ITALIC),
            ));
            lines.extend(wrap_hanging(
                spans,
                width,
                hang,
                Style::default().fg(theme::FAINT),
            ));
            index += 1;
            continue;
        }

        lines.extend(wrap(inline(raw, Style::default().fg(theme::FG)), width));
        index += 1;
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn task_marker(text: &str) -> (&'static str, &str) {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        ("\u{2610} ", rest)
    } else if let Some(rest) = text
        .strip_prefix("[x] ")
        .or_else(|| text.strip_prefix("[X] "))
    {
        ("\u{2611} ", rest)
    } else {
        ("\u{2022} ", text)
    }
}

fn heading(line: &str) -> Option<&str> {
    let level = line.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = line.get(level..)?;
    rest.strip_prefix(' ').map(str::trim_start)
}

fn ordered_item(line: &str) -> Option<(&str, &str)> {
    let digits = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = line.get(digits..)?;
    let rest = rest.strip_prefix(". ")?;
    Some((&line[..digits + 2], rest))
}

const TABLE_GAP: usize = 2;

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

fn table_block(lines: &[&str], start: usize, width: usize) -> Option<(Vec<Line<'static>>, usize)> {
    let header_line = *lines.get(start)?;
    if !header_line.contains('|') {
        return None;
    }
    let header = table_cells(header_line);
    if header.len() < 2 {
        return None;
    }
    let aligns = table_aligns(lines.get(start + 1)?)?;
    if aligns.len() != header.len() {
        return None;
    }

    let columns = header.len();
    let mut rows = Vec::new();
    let mut cursor = start + 2;
    while cursor < lines.len() {
        let line = lines[cursor];
        if line.trim().is_empty() || !line.contains('|') {
            break;
        }
        let mut cells = table_cells(line);
        cells.resize(columns, String::new());
        rows.push(cells);
        cursor += 1;
    }
    let consumed = cursor - start;

    let header_style = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(theme::FG);
    let mut widths = vec![1usize; columns];
    for (column, cell) in header.iter().enumerate() {
        widths[column] = widths[column].max(spans_width(&inline(cell, header_style)).min(width));
    }
    for row in &rows {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(spans_width(&inline(cell, body_style)).min(width));
        }
    }

    let mut total = widths.iter().sum::<usize>() + TABLE_GAP * (columns - 1);
    while total > width {
        let Some(column) = (0..columns)
            .filter(|column| widths[*column] > 1)
            .max_by_key(|column| widths[*column])
        else {
            break;
        };
        widths[column] -= 1;
        total -= 1;
    }

    if total > width {
        let mut out = wrap(inline(&header.join("  "), header_style), width);
        for row in &rows {
            out.extend(wrap(inline(&row.join("  "), body_style), width));
        }
        return Some((out, consumed));
    }

    let mut out = Vec::with_capacity(rows.len() + 1);
    out.push(table_row(&header, &widths, &aligns, header_style));
    for row in &rows {
        out.push(table_row(row, &widths, &aligns, body_style));
    }
    Some((out, consumed))
}

fn table_cells(line: &str) -> Vec<String> {
    let mut trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('|') {
        trimmed = rest;
    }
    if let Some(rest) = trimmed.strip_suffix('|') {
        trimmed = rest;
    }
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn table_aligns(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') || !line.contains('|') {
        return None;
    }
    let cells = table_cells(line);
    if cells.is_empty() {
        return None;
    }
    let mut aligns = Vec::with_capacity(cells.len());
    for cell in cells {
        let cell = cell.trim();
        let left = cell.starts_with(':');
        let right = cell.ends_with(':');
        let hyphens = cell.trim_matches(':');
        if hyphens.is_empty() || !hyphens.chars().all(|ch| ch == '-') {
            return None;
        }
        aligns.push(if left && right {
            Align::Center
        } else if right {
            Align::Right
        } else {
            Align::Left
        });
    }
    Some(aligns)
}

fn table_row(cells: &[String], widths: &[usize], aligns: &[Align], style: Style) -> Line<'static> {
    let mut spans = Vec::new();
    for (column, width) in widths.iter().enumerate() {
        let cell = cells.get(column).map(String::as_str).unwrap_or_default();
        spans.extend(fit_cell(inline(cell, style), *width, aligns[column], style));
        if column + 1 < widths.len() {
            spans.push(Span::styled(
                " ".repeat(TABLE_GAP),
                Style::default().fg(theme::FAINT),
            ));
        }
    }
    Line::from(spans)
}

fn fit_cell(
    spans: Vec<Span<'static>>,
    width: usize,
    align: Align,
    pad_style: Style,
) -> Vec<Span<'static>> {
    let spans = clip_spans(spans, width);
    let used = spans_width(&spans);
    let pad = width.saturating_sub(used);
    let (left, right) = match align {
        Align::Left => (0, pad),
        Align::Center => (pad / 2, pad - pad / 2),
        Align::Right => (pad, 0),
    };

    let mut out = Vec::new();
    if left > 0 {
        out.push(Span::styled(" ".repeat(left), pad_style));
    }
    out.extend(spans);
    if right > 0 {
        out.push(Span::styled(" ".repeat(right), pad_style));
    }
    out
}

fn clip_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let char_width = ch.width().unwrap_or(0);
            if used + char_width > width {
                break;
            }
            text.push(ch);
            used += char_width;
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
    }
    out
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| text_width(&span.content)).sum()
}

fn inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];

        if ch == '`' {
            let ticks = chars[index..]
                .iter()
                .take_while(|candidate| **candidate == '`')
                .count();
            if let Some(end) = find_tick_run(&chars, index + ticks, ticks) {
                flush(&mut buffer, &mut spans, base);
                let mut code: String = chars[index + ticks..end].iter().collect();
                if code.starts_with(' ') && code.ends_with(' ') && !code.trim().is_empty() {
                    code = code.chars().skip(1).collect();
                    code.pop();
                }
                spans.push(Span::styled(
                    code,
                    base.fg(theme::GREEN).bg(theme::PANEL_ALT),
                ));
                index = end + ticks;
                continue;
            }
            for _ in 0..ticks {
                buffer.push('`');
            }
            index += ticks;
            continue;
        }

        if ch == '*' && chars.get(index + 1) == Some(&'*') {
            if let Some(end) = find_sequence(&chars, index + 2, "**") {
                flush(&mut buffer, &mut spans, base);
                let bold: String = chars[index + 2..end].iter().collect();
                spans.push(Span::styled(bold, base.add_modifier(Modifier::BOLD)));
                index = end + 2;
                continue;
            }
        }

        if (ch == '*' || ch == '_') && (index == 0 || !chars[index - 1].is_alphanumeric()) {
            if let Some(end) = find_char(&chars, index + 1, ch) {
                if end > index + 1 {
                    flush(&mut buffer, &mut spans, base);
                    let italic: String = chars[index + 1..end].iter().collect();
                    spans.push(Span::styled(italic, base.add_modifier(Modifier::ITALIC)));
                    index = end + 1;
                    continue;
                }
            }
        }

        if ch == '[' {
            if let Some((label, end)) = link(&chars, index) {
                flush(&mut buffer, &mut spans, base);
                spans.push(Span::styled(
                    label,
                    base.fg(theme::ACCENT).add_modifier(Modifier::UNDERLINED),
                ));
                index = end;
                continue;
            }
        }

        buffer.push(ch);
        index += 1;
    }

    flush(&mut buffer, &mut spans, base);
    spans
}

fn flush(buffer: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
    if !buffer.is_empty() {
        spans.push(Span::styled(std::mem::take(buffer), style));
    }
}

fn find_char(chars: &[char], start: usize, target: char) -> Option<usize> {
    (start..chars.len()).find(|index| chars[*index] == target)
}

fn find_tick_run(chars: &[char], start: usize, count: usize) -> Option<usize> {
    let mut index = start;
    while index < chars.len() {
        if chars[index] == '`' {
            let run = chars[index..]
                .iter()
                .take_while(|candidate| **candidate == '`')
                .count();
            if run == count {
                return Some(index);
            }
            index += run;
        } else {
            index += 1;
        }
    }
    None
}

fn find_sequence(chars: &[char], start: usize, target: &str) -> Option<usize> {
    let target: Vec<char> = target.chars().collect();
    if target.is_empty() || start + target.len() > chars.len() {
        return None;
    }
    (start..=chars.len() - target.len())
        .find(|index| chars[*index..*index + target.len()] == target[..])
}

fn link(chars: &[char], start: usize) -> Option<(String, usize)> {
    let close = find_char(chars, start + 1, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = find_char(chars, close + 2, ')')?;
    let label: String = chars[start + 1..close].iter().collect();
    if label.is_empty() {
        return None;
    }
    Some((label, end + 1))
}

fn wrap_hanging(
    spans: Vec<Span<'static>>,
    width: usize,
    hang: usize,
    indent_style: Style,
) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let hang = hang.min(width.saturating_sub(1));
    let mut lines = Vec::new();
    let mut current: Vec<(Style, String)> = Vec::new();
    let mut used = 0usize;

    for span in spans {
        for ch in span.content.chars() {
            let char_width = ch.width().unwrap_or(0);
            if char_width == 0 {
                continue;
            }
            if used > 0 && used + char_width > width {
                lines.push(make_line(std::mem::take(&mut current)));
                used = 0;
                if hang > 0 {
                    current.push((indent_style, " ".repeat(hang)));
                    used = hang;
                }
            }
            if let Some((style, text)) = current.last_mut() {
                if *style == span.style {
                    text.push(ch);
                } else {
                    current.push((span.style, ch.to_string()));
                }
            } else {
                current.push((span.style, ch.to_string()));
            }
            used += char_width;
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(make_line(current));
    }
    lines
}

fn wrap(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut current: Vec<(Style, String)> = Vec::new();
    let mut used = 0usize;

    for span in spans {
        for ch in span.content.chars() {
            let char_width = ch.width().unwrap_or(0);
            if char_width == 0 {
                continue;
            }
            if used > 0 && used + char_width > width {
                lines.push(make_line(std::mem::take(&mut current)));
                used = 0;
            }
            if let Some((style, text)) = current.last_mut() {
                if *style == span.style {
                    text.push(ch);
                } else {
                    current.push((span.style, ch.to_string()));
                }
            } else {
                current.push((span.style, ch.to_string()));
            }
            used += char_width;
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(make_line(current));
    }
    lines
}

fn make_line(pieces: Vec<(Style, String)>) -> Line<'static> {
    Line::from(
        pieces
            .into_iter()
            .map(|(style, text)| Span::styled(text, style))
            .collect::<Vec<_>>(),
    )
}

pub fn code_block(text: &str, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let normalized = text.replace('\t', "    ");
    let mut lines = Vec::new();
    for raw in normalized.trim_end_matches('\n').split('\n') {
        lines.extend(code_line(raw, width));
    }
    lines
}

fn code_line(line: &str, width: usize) -> Vec<Line<'static>> {
    let style = Style::default().fg(theme::FG).bg(theme::PANEL_ALT);
    let inner_width = width.saturating_sub(2).max(1);
    let chunks = wrap_plain(line, inner_width);
    let mut out = Vec::new();

    for chunk in chunks {
        let mut spans = vec![Span::styled("  ", Style::default().bg(theme::PANEL_ALT))];
        let used = 2 + text_width(&chunk);
        spans.push(Span::styled(chunk, style));
        if used < width {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(theme::PANEL_ALT),
            ));
        }
        out.push(Line::from(spans));
    }

    if out.is_empty() {
        out.push(Line::from(Span::styled(
            " ".repeat(width),
            Style::default().bg(theme::PANEL_ALT),
        )));
    }
    out
}

fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;

    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > width {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(ch);
        used += char_width;
    }
    lines.push(current);
    lines
}

fn text_width(text: &str) -> usize {
    text.chars().map(|ch| ch.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_headings_lists_and_code() {
        let lines = render("# Title\n\n- **bold** item\n\n```\nlet x = 1;\n```", 40);
        let rendered = text(&lines);
        assert!(rendered.contains("Title"));
        assert!(!rendered.contains("# Title"));
        assert!(rendered.contains("\u{2022} bold item"));
        assert!(rendered.contains("let x = 1;"));
        assert!(lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn renders_tables_with_alignment() {
        let lines = render(
            "| item | qty |\n|:---|---:|\n| apple | 2 |\n| pear | 10 |",
            40,
        );
        let rendered = text(&lines);
        let rows: Vec<&str> = rendered.lines().collect();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].starts_with("item") && rows[0].contains("qty"));
        assert!(rows[1].contains("apple") && rows[1].ends_with(" 2"));
        assert!(rows[2].contains("pear") && rows[2].ends_with("10"));
        assert!(!rendered.contains("---"));
    }

    #[test]
    fn renders_variable_length_code_spans() {
        let lines = render("Use ``code with ` tick`` and `single` here", 60);
        let rendered = text(&lines);
        assert!(rendered.contains("code with ` tick"));
        assert!(rendered.contains("single"));
        assert!(!rendered.contains("``"));
        assert!(lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.content.contains("code with ` tick")
                && span.style.bg == Some(theme::PANEL_ALT)));
    }

    #[test]
    fn wraps_lists_with_hanging_indent() {
        let lines = render("- a long list item that wraps over several lines", 16);
        let rendered = text(&lines);
        let rows: Vec<&str> = rendered.lines().collect();
        assert!(rows[0].starts_with("\u{2022} "));
        assert!(rows.len() > 1);
        assert!(rows[1].starts_with("  "));
    }
}
