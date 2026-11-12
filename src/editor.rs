use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::style::Style;
use unicode_width::UnicodeWidthChar;

use crate::highlight::Highlighter;
use crate::predict::Predictor;

const TAB: usize = 4;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    selection: Option<(usize, usize)>,
}

struct HighlightResult {
    index: usize,
    version: u64,
    spans: Vec<Vec<(Style, String)>>,
}

#[derive(Clone, Copy, Debug)]
pub struct VisualRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

pub struct Buffer {
    pub path: PathBuf,
    pub rel: String,
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub scroll: usize,
    pub hscroll: usize,
    pub selection: Option<(usize, usize)>,
    pub dirty: bool,
    pub external_change: bool,
    pub highlight_dirty: bool,
    pub spans: Vec<Vec<(Style, String)>>,
    undo: Vec<Snapshot>,
    undo_group: Option<Instant>,
}

impl Buffer {
    fn remember(&mut self, coalesce: bool) {
        let now = Instant::now();
        let new_group = self.undo_group.map_or(true, |last| {
            now.duration_since(last) > Duration::from_millis(700)
        });
        if !coalesce || new_group {
            self.undo.push(self.snapshot());
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.undo_group = Some(now);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            selection: self.selection,
        }
    }

    pub fn matching_spans(&self, index: usize) -> Option<&[(Style, String)]> {
        let line = self.lines.get(index)?;
        let spans = self.spans.get(index)?;
        spans_match_line(line, spans).then_some(spans.as_slice())
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.lines = snapshot.lines;
        self.cursor_line = snapshot.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snapshot
            .cursor_col
            .min(char_count(&self.lines[self.cursor_line]));
        self.selection = snapshot.selection;
        self.dirty = true;
        self.highlight_dirty = true;
        self.undo_group = None;
    }
}

pub struct Editor {
    pub root: PathBuf,
    pub buffers: Vec<Buffer>,
    pub active: usize,
    pub wrap: bool,
    pub ghost: Option<String>,
    predictor: Predictor,
    highlighter: Arc<Highlighter>,
    highlight_tx: Sender<HighlightResult>,
    highlight_rx: Receiver<HighlightResult>,
    highlight_inflight: usize,
    last_edit: Instant,
    version: u64,
    prediction_key: Option<(usize, u64, usize, usize)>,
}

impl Editor {
    pub fn new(root: &Path) -> Self {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let (highlight_tx, highlight_rx) = mpsc::channel();
        Self {
            root,
            buffers: Vec::new(),
            active: 0,
            wrap: true,
            ghost: None,
            predictor: Predictor::load(),
            highlighter: Arc::new(Highlighter::new()),
            highlight_tx,
            highlight_rx,
            highlight_inflight: 0,
            last_edit: Instant::now(),
            version: 0,
            prediction_key: None,
        }
    }

    pub fn buffer(&self) -> Option<&Buffer> {
        self.buffers.get(self.active)
    }

    pub fn find_buffer(&self, path: &Path) -> Option<usize> {
        self.buffers.iter().position(|buffer| buffer.path == path)
    }

    pub fn rel_of(&self, path: &Path) -> String {
        let relative = path
            .strip_prefix(&self.root)
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        if relative.is_empty() {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or(relative)
        } else {
            relative
        }
    }

    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(index) = self.find_buffer(&path) {
            self.active = index;
            if !self.buffers[index].dirty {
                self.reload_at(index)?;
            }
            self.clear_prediction();
            return Ok(());
        }

        let lines = read_text(&path)?;
        let spans = self.highlighter.highlight(&path, &lines);
        let rel = self.rel_of(&path);
        self.buffers.push(Buffer {
            path,
            rel,
            lines,
            cursor_line: 0,
            cursor_col: 0,
            scroll: 0,
            hscroll: 0,
            selection: None,
            dirty: false,
            external_change: false,
            highlight_dirty: false,
            spans,
            undo: Vec::new(),
            undo_group: None,
        });
        self.active = self.buffers.len() - 1;
        self.clear_prediction();
        Ok(())
    }

    pub fn reload_at(&mut self, index: usize) -> Result<(), String> {
        let path = self.buffers.get(index).ok_or("no buffer")?.path.clone();
        let lines = read_text(&path)?;
        let buffer = &mut self.buffers[index];
        buffer.lines = lines;
        buffer.cursor_line = buffer.cursor_line.min(buffer.lines.len().saturating_sub(1));
        buffer.cursor_col = buffer
            .cursor_col
            .min(char_count(&buffer.lines[buffer.cursor_line]));
        buffer.scroll = buffer.scroll.min(buffer.lines.len().saturating_sub(1));
        buffer.dirty = false;
        buffer.external_change = false;
        buffer.highlight_dirty = true;
        buffer.undo.clear();
        buffer.undo_group = None;
        self.version += 1;
        self.clear_prediction();
        Ok(())
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    pub fn selection_text(&self) -> Option<String> {
        let ((start_line, start_col), (end_line, end_col)) = self.selection_range()?;
        let buffer = self.buffer()?;
        let mut text = String::new();
        for line_index in start_line..=end_line {
            let chars: Vec<char> = buffer.lines[line_index].chars().collect();
            let start = if line_index == start_line {
                start_col
            } else {
                0
            };
            let end = if line_index == end_line {
                end_col
            } else {
                chars.len()
            };
            let start = start.min(chars.len());
            let end = end.min(chars.len()).max(start);
            text.extend(&chars[start..end]);
            if line_index < end_line {
                text.push('\n');
            }
        }
        Some(text)
    }

    pub fn begin_mouse_selection(&mut self, row: usize, cells: usize, width: usize) {
        self.place_cursor_from_screen(row, cells, width);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.selection = Some((buffer.cursor_line, buffer.cursor_col));
        }
    }

    pub fn extend_mouse_selection(&mut self, row: usize, cells: usize, width: usize) {
        let anchor = self
            .buffers
            .get(self.active)
            .and_then(|buffer| buffer.selection);
        self.place_cursor_from_screen(row, cells, width);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.selection = anchor.or(Some((buffer.cursor_line, buffer.cursor_col)));
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(buffer) = self.buffers.get_mut(self.active) else {
            return false;
        };
        let Some(snapshot) = buffer.undo.pop() else {
            return false;
        };
        buffer.restore(snapshot);
        self.version += 1;
        self.clear_prediction();
        true
    }

    pub fn save_active(&mut self) -> Result<String, String> {
        let buffer = self.buffer().ok_or("no file open")?;
        fs::write(&buffer.path, buffer.lines.join("\n"))
            .map_err(|error| format!("save failed: {error}"))?;
        let rel = buffer.rel.clone();
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.dirty = false;
        }
        Ok(format!("saved {rel}"))
    }

    pub fn close_active(&mut self) -> Result<String, String> {
        let Some(buffer) = self.buffer() else {
            return Err("no file open".into());
        };
        if buffer.dirty {
            return Err("file has unsaved changes".into());
        }
        let rel = buffer.rel.clone();
        self.buffers.remove(self.active);
        if !self.buffers.is_empty() {
            self.active = self.active.min(self.buffers.len() - 1);
        } else {
            self.active = 0;
        }
        self.version += 1;
        self.clear_prediction();
        Ok(format!("closed {rel}"))
    }

    pub fn insert_char(&mut self, ch: char) {
        if self.buffers.get(self.active).is_none() {
            return;
        }
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.remember(true);
        }
        self.delete_selection_internal();
        let Some(buffer) = self.buffers.get_mut(self.active) else {
            return;
        };
        let mut chars: Vec<char> = buffer.lines[buffer.cursor_line].chars().collect();
        let col = buffer.cursor_col.min(chars.len());
        chars.insert(col, ch);
        buffer.lines[buffer.cursor_line] = chars.into_iter().collect();
        buffer.cursor_col = col + 1;
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    pub fn insert_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "");
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.newline();
            }
            for ch in part.chars() {
                self.insert_char(ch);
            }
        }
    }

    pub fn newline(&mut self) {
        if self.buffers.get(self.active).is_none() {
            return;
        }
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.remember(false);
        }
        self.delete_selection_internal();
        let Some(buffer) = self.buffers.get_mut(self.active) else {
            return;
        };
        let chars: Vec<char> = buffer.lines[buffer.cursor_line].chars().collect();
        let col = buffer.cursor_col.min(chars.len());
        let before: String = chars[..col].iter().collect();
        let after: String = chars[col..].iter().collect();
        let indent: String = before.chars().take_while(|ch| ch.is_whitespace()).collect();
        let opens_block = matches!(
            before.trim_end().chars().last(),
            Some('{') | Some('(') | Some('[') | Some(':')
        );
        let extra = if opens_block && after.trim().is_empty() {
            "    "
        } else {
            ""
        };
        let next_line = format!("{indent}{extra}{after}");
        let next_col = indent.chars().count() + extra.chars().count();

        let line = buffer.cursor_line;
        buffer.lines[line] = before;
        buffer.lines.insert(line + 1, next_line);
        if line < buffer.spans.len() {
            buffer.spans.insert(line + 1, Vec::new());
        }
        buffer.cursor_line += 1;
        buffer.cursor_col = next_col;
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    pub fn backspace(&mut self) {
        let has_selection = self.selection_range().is_some();
        let can_change = has_selection
            || self
                .buffers
                .get(self.active)
                .map(|buffer| buffer.cursor_col > 0 || buffer.cursor_line > 0)
                .unwrap_or(false);
        if !can_change {
            return;
        }
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.remember(false);
        }
        if self.delete_selection_internal() {
            if let Some(buffer) = self.buffers.get_mut(self.active) {
                buffer.dirty = true;
                buffer.highlight_dirty = true;
            }
            self.version += 1;
            self.touch();
            return;
        }
        let Some(buffer) = self.buffers.get_mut(self.active) else {
            return;
        };
        if buffer.cursor_col > 0 {
            let mut chars: Vec<char> = buffer.lines[buffer.cursor_line].chars().collect();
            let col = buffer.cursor_col.min(chars.len());
            chars.remove(col - 1);
            buffer.lines[buffer.cursor_line] = chars.into_iter().collect();
            buffer.cursor_col = col - 1;
        } else if buffer.cursor_line > 0 {
            let removed = buffer.cursor_line;
            let current = buffer.lines.remove(removed);
            if removed < buffer.spans.len() {
                buffer.spans.remove(removed);
            }
            let previous = &mut buffer.lines[removed - 1];
            let col = previous.chars().count();
            previous.push_str(&current);
            buffer.cursor_line -= 1;
            buffer.cursor_col = col;
        } else {
            return;
        }
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    pub fn delete(&mut self) {
        let has_selection = self.selection_range().is_some();
        let can_change = has_selection
            || self
                .buffers
                .get(self.active)
                .map(|buffer| {
                    buffer.cursor_col < char_count(&buffer.lines[buffer.cursor_line])
                        || buffer.cursor_line + 1 < buffer.lines.len()
                })
                .unwrap_or(false);
        if !can_change {
            return;
        }
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.remember(false);
        }
        if self.delete_selection_internal() {
            if let Some(buffer) = self.buffers.get_mut(self.active) {
                buffer.dirty = true;
                buffer.highlight_dirty = true;
            }
            self.version += 1;
            self.touch();
            return;
        }
        let Some(buffer) = self.buffers.get_mut(self.active) else {
            return;
        };
        let length = char_count(&buffer.lines[buffer.cursor_line]);
        if buffer.cursor_col < length {
            let mut chars: Vec<char> = buffer.lines[buffer.cursor_line].chars().collect();
            chars.remove(buffer.cursor_col);
            buffer.lines[buffer.cursor_line] = chars.into_iter().collect();
        } else if buffer.cursor_line + 1 < buffer.lines.len() {
            let next = buffer.lines.remove(buffer.cursor_line + 1);
            if buffer.cursor_line + 1 < buffer.spans.len() {
                buffer.spans.remove(buffer.cursor_line + 1);
            }
            buffer.lines[buffer.cursor_line].push_str(&next);
        } else {
            return;
        }
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    pub fn indent(&mut self) {
        if self.selection_range().is_some() {
            if let Some(buffer) = self.buffers.get_mut(self.active) {
                buffer.remember(false);
            }
            self.indent_selection();
            return;
        }
        for _ in 0..4 {
            self.insert_char(' ');
        }
    }

    pub fn dedent(&mut self) {
        if self.selection_range().is_some() {
            if let Some(buffer) = self.buffers.get_mut(self.active) {
                buffer.remember(false);
            }
            self.dedent_selection();
            return;
        }
        let remove = self
            .buffer()
            .map(|buffer| {
                let line = &buffer.lines[buffer.cursor_line];
                if line.starts_with('\t') {
                    1
                } else {
                    line.chars().take_while(|ch| *ch == ' ').count().min(4)
                }
            })
            .unwrap_or(0);
        if remove == 0 {
            return;
        }
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.remember(false);
        }
        self.delete_selection_internal();
        let Some(buffer) = self.buffers.get_mut(self.active) else {
            return;
        };
        let line = &mut buffer.lines[buffer.cursor_line];
        let rest: String = line.chars().skip(remove).collect();
        *line = rest;
        buffer.cursor_col = buffer.cursor_col.saturating_sub(remove);
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    pub fn move_left(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            if buffer.cursor_col > 0 {
                buffer.cursor_col -= 1;
            } else if buffer.cursor_line > 0 {
                buffer.cursor_line -= 1;
                buffer.cursor_col = char_count(&buffer.lines[buffer.cursor_line]);
            }
            self.touch();
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            let length = char_count(&buffer.lines[buffer.cursor_line]);
            if buffer.cursor_col < length {
                buffer.cursor_col += 1;
            } else if buffer.cursor_line + 1 < buffer.lines.len() {
                buffer.cursor_line += 1;
                buffer.cursor_col = 0;
            }
            self.touch();
        }
    }

    pub fn move_up(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            if buffer.cursor_line > 0 {
                buffer.cursor_line -= 1;
                buffer.cursor_col = buffer
                    .cursor_col
                    .min(char_count(&buffer.lines[buffer.cursor_line]));
            }
            self.touch();
        }
    }

    pub fn move_down(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            if buffer.cursor_line + 1 < buffer.lines.len() {
                buffer.cursor_line += 1;
                buffer.cursor_col = buffer
                    .cursor_col
                    .min(char_count(&buffer.lines[buffer.cursor_line]));
            }
            self.touch();
        }
    }

    pub fn move_home(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.cursor_col = 0;
            self.touch();
        }
    }

    pub fn move_end(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.cursor_col = char_count(&buffer.lines[buffer.cursor_line]);
            self.touch();
        }
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            let mut chars: Vec<char> = buffer.lines[buffer.cursor_line].chars().collect();
            let mut col = buffer.cursor_col.min(chars.len());
            while col > 0 && !is_word(chars[col - 1]) {
                col -= 1;
            }
            while col > 0 && is_word(chars[col - 1]) {
                col -= 1;
            }
            buffer.cursor_col = col;
            chars.clear();
            self.touch();
        }
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            let chars: Vec<char> = buffer.lines[buffer.cursor_line].chars().collect();
            let mut col = buffer.cursor_col.min(chars.len());
            while col < chars.len() && is_word(chars[col]) {
                col += 1;
            }
            while col < chars.len() && !is_word(chars[col]) {
                col += 1;
            }
            buffer.cursor_col = col;
            self.touch();
        }
    }

    pub fn move_page(&mut self, lines: isize, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            let next = buffer.cursor_line as isize + lines;
            buffer.cursor_line =
                next.clamp(0, buffer.lines.len().saturating_sub(1) as isize) as usize;
            buffer.cursor_col = buffer
                .cursor_col
                .min(char_count(&buffer.lines[buffer.cursor_line]));
            self.touch();
        }
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.cursor_line = 0;
            buffer.cursor_col = 0;
            self.touch();
        }
    }

    pub fn doc_end(&mut self, extend: bool) {
        self.prepare_move(extend);
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.cursor_line = buffer.lines.len().saturating_sub(1);
            buffer.cursor_col = char_count(&buffer.lines[buffer.cursor_line]);
            self.touch();
        }
    }

    pub fn switch_buffer(&mut self, delta: isize) {
        if self.buffers.len() < 2 {
            return;
        }
        let length = self.buffers.len() as isize;
        let next = ((self.active as isize + delta).rem_euclid(length)) as usize;
        self.select_buffer(next);
    }

    pub fn select_buffer(&mut self, index: usize) {
        if index >= self.buffers.len() {
            return;
        }
        self.active = index;
        self.buffers[self.active].undo_group = None;
        self.clear_prediction();
    }

    pub fn toggle_wrap(&mut self) -> bool {
        self.wrap = !self.wrap;
        self.wrap
    }

    pub fn visual_rows(&self, width: usize) -> Vec<VisualRow> {
        self.buffer()
            .map(|buffer| build_rows(&buffer.lines, self.wrap, width))
            .unwrap_or_default()
    }

    pub fn scroll_by(&mut self, delta: isize, width: usize) {
        let Some(buffer) = self.buffer() else {
            return;
        };
        let total = if self.wrap {
            build_rows(&buffer.lines, true, width).len()
        } else {
            buffer.lines.len()
        };
        let buffer = &mut self.buffers[self.active];
        let next = buffer.scroll as isize + delta;
        buffer.scroll = next.clamp(0, total.saturating_sub(1) as isize) as usize;
    }

    pub fn ensure_visible(&mut self, height: usize, width: usize) {
        if height == 0 || width == 0 {
            return;
        }
        let Some(buffer) = self.buffer() else {
            return;
        };

        if self.wrap {
            let rows = build_rows(&buffer.lines, true, width);
            let cursor_row = row_for_cursor(&rows, buffer.cursor_line, buffer.cursor_col);
            let buffer = &mut self.buffers[self.active];
            buffer.hscroll = 0;
            if cursor_row < buffer.scroll {
                buffer.scroll = cursor_row;
            }
            if cursor_row >= buffer.scroll + height {
                buffer.scroll = cursor_row + 1 - height;
            }
            buffer.scroll = buffer.scroll.min(rows.len().saturating_sub(1));
            return;
        }

        let cursor_line = buffer.cursor_line;
        let cursor_col = buffer.cursor_col;
        let chars: Vec<char> = buffer.lines[cursor_line].chars().collect();
        let prefix: String = chars[..cursor_col.min(chars.len())].iter().collect();
        let column = display_width(&prefix);
        let buffer = &mut self.buffers[self.active];
        if cursor_line < buffer.scroll {
            buffer.scroll = cursor_line;
        }
        if cursor_line >= buffer.scroll + height {
            buffer.scroll = cursor_line + 1 - height;
        }
        if column < buffer.hscroll {
            buffer.hscroll = column;
        }
        if column >= buffer.hscroll + width {
            buffer.hscroll = column + 1 - width;
        }
    }

    pub fn cursor_visual(&self, width: usize) -> Option<(usize, usize)> {
        let buffer = self.buffer()?;
        let rows = build_rows(&buffer.lines, self.wrap, width);
        let row = row_for_cursor(&rows, buffer.cursor_line, buffer.cursor_col);
        let visual = rows.get(row)?;
        let chars: Vec<char> = buffer.lines[visual.line].chars().collect();
        let start = visual.start.min(chars.len());
        let end = visual.end.min(chars.len()).max(start);
        let col = buffer.cursor_col.min(end).max(start);
        let prefix: String = chars[start..col].iter().collect();
        Some((row, display_width(&prefix)))
    }

    pub fn place_cursor_from_screen(&mut self, row: usize, cells: usize, width: usize) {
        let Some(buffer) = self.buffer() else {
            return;
        };
        let (line, start, end, target) = if self.wrap {
            let rows = build_rows(&buffer.lines, true, width);
            let visual = rows
                .get(row)
                .or_else(|| rows.last())
                .copied()
                .unwrap_or(VisualRow {
                    line: 0,
                    start: 0,
                    end: 0,
                });
            (visual.line, visual.start, visual.end, cells)
        } else {
            let line = row.min(buffer.lines.len().saturating_sub(1));
            let end = char_count(&buffer.lines[line]);
            (line, 0, end, buffer.hscroll + cells)
        };
        let mut column = 0;
        let mut best = start;
        for (index, ch) in buffer.lines[line]
            .chars()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            if column > target {
                break;
            }
            best = index;
            if ch == '\t' {
                column = (column / TAB + 1) * TAB;
            } else {
                column += ch.width().unwrap_or(0);
            }
        }
        let buffer = &mut self.buffers[self.active];
        buffer.cursor_line = line;
        buffer.cursor_col = best;
        buffer.undo_group = None;
        self.touch();
    }

    pub fn tick(&mut self, now: Instant) {
        self.poll_highlight();
        let highlight_dirty = self
            .buffers
            .get(self.active)
            .map(|buffer| buffer.highlight_dirty)
            .unwrap_or(false);
        if highlight_dirty
            && self.highlight_inflight == 0
            && now.duration_since(self.last_edit) >= Duration::from_millis(250)
        {
            self.start_highlight();
        }

        if self.buffers.is_empty() {
            self.ghost = None;
            self.prediction_key = None;
            return;
        }
        if now.duration_since(self.last_edit) < Duration::from_millis(180) {
            return;
        }

        let buffer = &self.buffers[self.active];
        let key = (
            self.active,
            self.version,
            buffer.cursor_line,
            buffer.cursor_col,
        );
        if self.prediction_key == Some(key) {
            return;
        }
        self.prediction_key = Some(key);
        self.ghost = self.compute_ghost();
    }

    pub fn accept_prediction(&mut self) -> bool {
        if self.has_selection() {
            return false;
        }
        let Some(ghost) = self.ghost.take() else {
            return false;
        };
        if let Some(buffer) = self.buffer() {
            let line = buffer.lines[buffer.cursor_line].clone();
            let full = format!("{line}{ghost}");
            self.predictor.learn(&full);
        }
        for ch in ghost.chars() {
            self.insert_char(ch);
        }
        true
    }

    fn compute_ghost(&self) -> Option<String> {
        let buffer = self.buffers.get(self.active)?;
        if buffer.selection.is_some() {
            return None;
        }
        let line = buffer.lines.get(buffer.cursor_line)?;
        if buffer.cursor_col != char_count(line) {
            return None;
        }
        if buffer.lines.len() > 60_000 {
            return None;
        }

        let mut corpus: Vec<&str> = buffer.lines.iter().map(String::as_str).collect();
        for (index, other) in self.buffers.iter().enumerate() {
            if index == self.active {
                continue;
            }
            for candidate in &other.lines {
                corpus.push(candidate);
                if corpus.len() >= 40_000 {
                    break;
                }
            }
            if corpus.len() >= 40_000 {
                break;
            }
        }

        self.predictor.suggest(corpus, line, buffer.cursor_col)
    }

    fn start_highlight(&mut self) {
        let Some(buffer) = self.buffers.get(self.active) else {
            return;
        };
        let index = self.active;
        let version = self.version;
        let path = buffer.path.clone();
        let lines = buffer.lines.clone();
        let highlighter = Arc::clone(&self.highlighter);
        let tx = self.highlight_tx.clone();
        self.highlight_inflight += 1;
        thread::spawn(move || {
            let spans = highlighter.highlight(&path, &lines);
            let _ = tx.send(HighlightResult {
                index,
                version,
                spans,
            });
        });
    }

    fn poll_highlight(&mut self) {
        while let Ok(result) = self.highlight_rx.try_recv() {
            self.highlight_inflight = self.highlight_inflight.saturating_sub(1);
            if result.version != self.version {
                continue;
            }
            if let Some(buffer) = self.buffers.get_mut(result.index) {
                buffer.spans = result.spans;
                buffer.highlight_dirty = false;
            }
        }
    }

    fn prepare_move(&mut self, extend: bool) {
        if let Some(buffer) = self.buffers.get_mut(self.active) {
            buffer.undo_group = None;
            if extend {
                if buffer.selection.is_none() {
                    buffer.selection = Some((buffer.cursor_line, buffer.cursor_col));
                }
            } else {
                buffer.selection = None;
            }
        }
    }

    fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let buffer = self.buffer()?;
        let anchor = buffer.selection?;
        let line_count = buffer.lines.len();
        if line_count == 0 {
            return None;
        }
        let anchor_line = anchor.0.min(line_count - 1);
        let anchor_col = anchor.1.min(char_count(&buffer.lines[anchor_line]));
        let start = (anchor_line, anchor_col);
        let end = (buffer.cursor_line, buffer.cursor_col);
        if start == end {
            None
        } else if start < end {
            Some((start, end))
        } else {
            Some((end, start))
        }
    }

    fn delete_selection_internal(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            if let Some(buffer) = self.buffers.get_mut(self.active) {
                buffer.selection = None;
            }
            return false;
        };

        let buffer = &mut self.buffers[self.active];
        let start_chars: Vec<char> = buffer.lines[start.0].chars().collect();
        let end_chars: Vec<char> = buffer.lines[end.0].chars().collect();
        let start_col = start.1.min(start_chars.len());
        let end_col = end.1.min(end_chars.len());
        let prefix: String = start_chars[..start_col].iter().collect();
        let suffix: String = end_chars[end_col..].iter().collect();
        let merged = format!("{prefix}{suffix}");
        buffer
            .lines
            .splice(start.0..=end.0, std::iter::once(merged));
        if start.0 < buffer.spans.len() {
            let remove_end = end.0.min(buffer.spans.len() - 1);
            buffer
                .spans
                .splice(start.0..=remove_end, std::iter::once(Vec::new()));
        }
        buffer.cursor_line = start.0;
        buffer.cursor_col = prefix.chars().count();
        buffer.selection = None;
        true
    }

    fn indent_selection(&mut self) {
        let Some((start, end)) = self.selection_range() else {
            return;
        };
        let buffer = &mut self.buffers[self.active];
        for index in start.0..=end.0 {
            buffer.lines[index].insert_str(0, "    ");
        }
        buffer.cursor_col += 4;
        if let Some(anchor) = buffer.selection.as_mut() {
            if anchor.0 >= start.0 && anchor.0 <= end.0 {
                anchor.1 += 4;
            }
        }
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    fn dedent_selection(&mut self) {
        let Some((start, end)) = self.selection_range() else {
            return;
        };
        let buffer = &mut self.buffers[self.active];
        let cursor_line = buffer.cursor_line;
        let mut cursor_removed = 0;
        for index in start.0..=end.0 {
            let remove = {
                let line = &buffer.lines[index];
                if line.starts_with('\t') {
                    1
                } else {
                    line.chars().take_while(|ch| *ch == ' ').count().min(4)
                }
            };
            if remove > 0 {
                buffer.lines[index] = buffer.lines[index].chars().skip(remove).collect();
            }
            if index == cursor_line {
                cursor_removed = remove;
            }
            if let Some(anchor) = buffer.selection.as_mut() {
                if anchor.0 == index {
                    anchor.1 = anchor.1.saturating_sub(remove);
                }
            }
        }
        buffer.cursor_col = buffer.cursor_col.saturating_sub(cursor_removed);
        buffer.dirty = true;
        buffer.highlight_dirty = true;
        self.version += 1;
        self.touch();
    }

    fn clear_prediction(&mut self) {
        self.ghost = None;
        self.prediction_key = None;
        self.last_edit = Instant::now();
    }

    fn touch(&mut self) {
        self.ghost = None;
        self.prediction_key = None;
        self.last_edit = Instant::now();
    }
}

pub fn display_width(text: &str) -> usize {
    let mut column = 0;
    for ch in text.chars() {
        if ch == '\t' {
            column = (column / TAB + 1) * TAB;
        } else {
            column += ch.width().unwrap_or(0);
        }
    }
    column
}

pub fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn row_for_cursor(rows: &[VisualRow], line: usize, col: usize) -> usize {
    let mut last = 0;
    for (index, row) in rows.iter().enumerate() {
        if row.line == line {
            last = index;
            if col >= row.start && col < row.end {
                return index;
            }
        }
    }
    last
}

fn build_rows(lines: &[String], wrap: bool, width: usize) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        if !wrap || width == 0 || chars.is_empty() {
            rows.push(VisualRow {
                line: line_index,
                start: 0,
                end: chars.len(),
            });
            continue;
        }

        let mut start = 0;
        while start < chars.len() {
            let mut used = 0;
            let mut last_break = None;
            let mut index = start;
            while index < chars.len() {
                let char_width = if chars[index] == '\t' {
                    TAB - used % TAB
                } else {
                    chars[index].width().unwrap_or(0)
                };
                if used + char_width > width && index > start {
                    break;
                }
                used += char_width;
                index += 1;
                if chars[index - 1].is_whitespace() {
                    last_break = Some(index);
                }
            }

            let end = if index < chars.len() {
                last_break.filter(|end| *end > start).unwrap_or(index)
            } else {
                index
            };
            rows.push(VisualRow {
                line: line_index,
                start,
                end,
            });
            start = end;
        }
    }
    rows
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn spans_match_line(line: &str, spans: &[(Style, String)]) -> bool {
    let mut chars = line.chars();
    for (_, text) in spans {
        for ch in text.chars() {
            if chars.next() != Some(ch) {
                return false;
            }
        }
    }
    chars.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_copies_across_lines() {
        let mut editor = Editor::new(Path::new("."));
        editor.buffers.push(Buffer {
            path: PathBuf::from("test.txt"),
            rel: "test.txt".to_string(),
            lines: vec!["abc".to_string(), "def".to_string()],
            cursor_line: 1,
            cursor_col: 2,
            scroll: 0,
            hscroll: 0,
            selection: Some((0, 1)),
            dirty: false,
            external_change: false,
            highlight_dirty: false,
            spans: Vec::new(),
            undo: Vec::new(),
            undo_group: None,
        });
        editor.active = 0;

        assert!(editor.has_selection());
        assert_eq!(editor.selection_text().as_deref(), Some("bc\nde"));
    }

    #[test]
    fn undo_restores_grouped_typing() {
        let mut editor = Editor::new(Path::new("."));
        editor.buffers.push(Buffer {
            path: PathBuf::from("test.txt"),
            rel: "test.txt".to_string(),
            lines: vec!["start".to_string()],
            cursor_line: 0,
            cursor_col: 5,
            scroll: 0,
            hscroll: 0,
            selection: None,
            dirty: false,
            external_change: false,
            highlight_dirty: false,
            spans: Vec::new(),
            undo: Vec::new(),
            undo_group: None,
        });
        editor.active = 0;

        editor.insert_text(" more");
        assert_eq!(editor.buffer().unwrap().lines[0], "start more");
        assert!(editor.undo());
        assert_eq!(editor.buffer().unwrap().lines[0], "start");
        assert!(!editor.undo());
    }

    #[test]
    fn only_changed_lines_lose_cached_spans() {
        let mut editor = Editor::new(Path::new("."));
        editor.buffers.push(Buffer {
            path: PathBuf::from("test.rs"),
            rel: "test.rs".to_string(),
            lines: vec!["let a = 1;".to_string(), "let b = 2;".to_string()],
            cursor_line: 0,
            cursor_col: 0,
            scroll: 0,
            hscroll: 0,
            selection: None,
            dirty: false,
            external_change: false,
            highlight_dirty: false,
            spans: vec![
                vec![(Style::default(), "let a = 1;".to_string())],
                vec![(Style::default(), "let b = 2;".to_string())],
            ],
            undo: Vec::new(),
            undo_group: None,
        });
        editor.active = 0;

        editor.insert_char('Q');
        let buffer = editor.buffer().unwrap();
        assert!(buffer.matching_spans(0).is_none());
        assert!(buffer.matching_spans(1).is_some());

        editor.newline();
        let buffer = editor.buffer().unwrap();
        assert_eq!(buffer.lines, ["Q", "let a = 1;", "let b = 2;"]);
        assert!(buffer.matching_spans(1).is_none());
        assert!(buffer.matching_spans(2).is_some());
    }
}

fn read_text(path: &Path) -> Result<Vec<String>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err("file is too large to open".into());
    }
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.contains(&0) {
        return Err("binary file".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "file is not utf-8".to_string())?;
    Ok(text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect())
}
