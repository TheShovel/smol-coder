use std::path::Path;

use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, DiffKind, Entry, FileDialog, Focus, PiState, ResizeEdge, Sidebar, ToolBodyKind, ToolState,
    Track, TrackKind,
};
use crate::editor::{display_width, Buffer};
use crate::theme;
use crate::todo::{format_duration, TodoStatus, TodoTask};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::base()), area);

    if area.width < 60 || area.height < 8 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "terminal too small",
                Style::default().fg(theme::DIM),
            )))
            .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    app.ui.status = status;
    app.ui.main = main;

    let min_left = 12u16;
    let min_editor = 16u16;
    let min_agent = 18u16;
    let default_left =
        ((main.width / 5).clamp(16, 28)).min(main.width.saturating_sub(min_editor + min_agent));
    let reserved_agent = if app.agent_hidden { 0 } else { min_agent };
    let max_left = main
        .width
        .saturating_sub(min_editor.saturating_add(reserved_agent))
        .max(min_left);
    let mut left = app
        .left_width
        .unwrap_or(default_left)
        .min(max_left)
        .max(min_left.min(max_left));
    left = left.min(main.width.saturating_sub(min_editor));
    app.left_width = Some(left);

    let max_right = main.width.saturating_sub(left).saturating_sub(min_editor);
    let right = if app.agent_hidden {
        0
    } else {
        let max_right = max_right.max(min_agent.min(main.width));
        let stored = app.right_width.unwrap_or((main.width / 3).clamp(24, 44));
        let right = stored.min(max_right).max(min_agent.min(max_right));
        app.right_width = Some(right);
        right
    };
    let editor_width = main.width.saturating_sub(left + right);

    let [tree_rect, editor_rect, agent_rect] = Layout::horizontal([
        Constraint::Length(left),
        Constraint::Length(editor_width),
        Constraint::Length(right),
    ])
    .areas(main);

    app.ui.tree_divider = (left > 0).then_some(main.x + left - 1);
    app.ui.agent_divider = (right > 0).then_some(main.x + left + editor_width);
    app.ui.agent = Rect::default();
    app.ui.agent_thinking.clear();
    app.ui.diff_header = Rect::default();

    draw_sidebar(frame, tree_rect, app);
    draw_editor(frame, editor_rect, app);
    if !app.agent_hidden {
        draw_agent(frame, agent_rect, app);
    }
    draw_menu(frame, main, app);
    draw_context_menu(frame, main, app);
    draw_session_history(frame, main, app);
    draw_picker(frame, main, app);
    draw_file_dialog(frame, main, app);
    draw_status(frame, status, app);
}

fn draw_sidebar(frame: &mut Frame, rect: Rect, app: &mut App) {
    let border = if matches!(app.resizing, Some(ResizeEdge::Sidebar)) {
        theme::ACCENT
    } else if app.hover_edge == Some(ResizeEdge::Sidebar) {
        theme::DIM
    } else {
        theme::BORDER
    };
    let block = Block::default()
        .style(Style::default().bg(theme::PANEL))
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(border));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    app.ui.tree = inner;

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    match app.sidebar {
        Sidebar::Files => {
            let focused = app.focus == Focus::Tree;
            let title = app
                .tree
                .nodes
                .first()
                .map(|node| node.name.clone())
                .unwrap_or_default();
            let style = if focused {
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::FAINT)
                    .add_modifier(Modifier::BOLD)
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    clip_display(&title, inner.width as usize),
                    style,
                )))
                .style(Style::default().bg(theme::PANEL)),
                Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: 1,
                },
            );

            let body = Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: inner.height - 1,
            };
            app.ui.tree_body = body;
            draw_tree_rows(frame, body, app);
        }
        Sidebar::Git => {
            let mut spans = vec![
                Span::styled(
                    "git",
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ", Style::default().fg(theme::FAINT)),
                Span::styled(
                    clip_display(&app.git.branch, inner.width as usize / 2),
                    Style::default().fg(theme::DIM),
                ),
            ];
            if app.git.pushing {
                spans.push(Span::styled(
                    "  pushing",
                    Style::default().fg(theme::ACCENT),
                ));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::PANEL)),
                Rect {
                    x: inner.x,
                    y: inner.y,
                    width: inner.width,
                    height: 1,
                },
            );

            let reserve = if app.git.repo && inner.height >= 3 {
                1
            } else {
                0
            };
            let body = Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: inner.height.saturating_sub(1 + reserve),
            };
            app.ui.tree_body = body;
            draw_git_rows(frame, body, app);

            if reserve == 1 {
                let input = Rect {
                    x: inner.x,
                    y: inner.y + inner.height - 1,
                    width: inner.width,
                    height: 1,
                };
                app.ui.git_input = input;
                draw_git_input(frame, input, app);
            } else {
                app.ui.git_input = Rect::default();
            }
        }
    }
}

fn draw_tree_rows(frame: &mut Frame, body: Rect, app: &mut App) {
    let height = body.height as usize;
    if height == 0 {
        return;
    }

    if app.tree.visible.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "empty project",
                Style::default().fg(theme::FAINT),
            )))
            .style(Style::default().bg(theme::PANEL)),
            body,
        );
        return;
    }

    if app.tree.selected < app.tree.scroll {
        app.tree.scroll = app.tree.selected;
    }
    if app.tree.selected >= app.tree.scroll + height {
        app.tree.scroll = app.tree.selected + 1 - height;
    }

    let focused = app.focus == Focus::Tree;
    let width = body.width as usize;
    let hover_row = app.hover.and_then(|(x, y)| {
        (x >= body.x && x < body.x + body.width && y >= body.y && y < body.y + body.height)
            .then_some((y - body.y) as usize + app.tree.scroll)
    });
    let mut lines = Vec::with_capacity(height);

    for position in app.tree.scroll..app.tree.visible.len().min(app.tree.scroll + height) {
        let node = &app.tree.nodes[app.tree.visible[position]];
        let selected = position == app.tree.selected;
        let hovered = hover_row == Some(position);
        let bg = if selected {
            if focused {
                theme::SELECT
            } else {
                theme::PANEL_ALT
            }
        } else if hovered {
            theme::HOVER
        } else {
            theme::PANEL
        };

        let indent = "  ".repeat(node.depth);
        let marker = if node.is_dir {
            if node.expanded {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            ""
        };
        let prefix = format!("{indent}{marker}");
        let prefix_width = display_width(&prefix);
        let name_width = width.saturating_sub(prefix_width);
        let name = clip_display(&node.name, name_width);
        let name_width = display_width(&name);

        let mut spans = vec![
            Span::styled(prefix.clone(), Style::default().fg(theme::DIM).bg(bg)),
            Span::styled(name, Style::default().fg(theme::FG).bg(bg)),
        ];
        let used = prefix_width + name_width;
        if used < width {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        body,
    );
}

fn draw_git_rows(frame: &mut Frame, body: Rect, app: &mut App) {
    let height = body.height as usize;
    if height == 0 {
        return;
    }

    if !app.git.available {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "git not found",
                Style::default().fg(theme::RED),
            )))
            .style(Style::default().bg(theme::PANEL)),
            body,
        );
        return;
    }
    if !app.git.repo {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no repository",
                Style::default().fg(theme::FAINT),
            )))
            .style(Style::default().bg(theme::PANEL)),
            body,
        );
        return;
    }
    if app.git.changes.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no changes",
                Style::default().fg(theme::FAINT),
            )))
            .style(Style::default().bg(theme::PANEL)),
            body,
        );
        return;
    }

    if app.git.selected < app.git.scroll {
        app.git.scroll = app.git.selected;
    }
    if app.git.selected >= app.git.scroll + height {
        app.git.scroll = app.git.selected + 1 - height;
    }

    let focused = app.focus == Focus::Tree;
    let width = body.width as usize;
    let mut lines = Vec::with_capacity(height);

    for position in app.git.scroll..app.git.changes.len().min(app.git.scroll + height) {
        let change = &app.git.changes[position];
        let selected = position == app.git.selected;
        let bg = if selected {
            if focused {
                theme::SELECT
            } else {
                theme::PANEL_ALT
            }
        } else {
            theme::PANEL
        };
        let label = change.status.trim().to_string();
        let label = format!("{label:>2} ");
        let color = status_color(&label);
        let path = clip_display(&change.path, width.saturating_sub(3));
        let path_width = display_width(&path);

        let mut spans = vec![
            Span::styled(label, Style::default().fg(color).bg(bg)),
            Span::styled(path, Style::default().fg(theme::FG).bg(bg)),
        ];
        let used = 3 + path_width;
        if used < width {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        body,
    );
}

fn draw_git_input(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Tree && app.git.input_focus;
    let prompt = if focused {
        Style::default().fg(theme::ACCENT)
    } else {
        Style::default().fg(theme::FAINT)
    };
    let mut spans = vec![Span::styled("> ", prompt)];
    let width = area.width as usize;
    let available = width.saturating_sub(2);

    if app.git.input.is_empty() {
        spans.push(Span::styled(
            "commit message",
            Style::default()
                .fg(theme::FAINT)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let chars: Vec<char> = app.git.input.chars().collect();
        let prefix: String = chars[..app.git.input_cursor.min(chars.len())]
            .iter()
            .collect();
        let prefix_width = display_width(&prefix);
        let start = prefix_width.saturating_sub(available.saturating_sub(1));
        let visible = clip_from(&app.git.input, start, available);
        spans.push(Span::styled(visible, Style::default().fg(theme::FG)));
        if focused {
            let x = area.x as usize + 2 + prefix_width.saturating_sub(start);
            if x < (area.x + area.width) as usize {
                frame.set_cursor_position(Position::new(x as u16, area.y));
            }
        }
    }

    if focused && app.git.input.is_empty() {
        frame.set_cursor_position(Position::new(area.x + 2, area.y));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::PANEL)),
        area,
    );
}

fn draw_editor(frame: &mut Frame, rect: Rect, app: &mut App) {
    frame.render_widget(Block::default().style(Style::default().bg(theme::BG)), rect);
    app.ui.editor = rect;

    let inner = Rect {
        x: rect.x + 1,
        y: rect.y,
        width: rect.width.saturating_sub(2),
        height: rect.height,
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let header = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };

    app.ui.tabs.clear();
    app.ui.unsupported_button = Rect::default();
    if app.diff.is_some() {
        draw_diff(frame, inner, app);
        return;
    }
    if let Some(path) = app.unsupported.clone() {
        draw_unsupported(frame, inner, header, app, &path);
        return;
    }

    if let Some(buffer) = app.editor.buffer() {
        let focused = app.focus == Focus::Editor;
        let style = if focused {
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::DIM)
        };
        let track = app.track_label_for(&buffer.path).unwrap_or_default();
        let right = if track.is_empty() {
            String::new()
        } else {
            format!("pi {track}")
        };
        let right_width = display_width(&right).min(inner.width as usize / 2) as u16;
        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(header);

        let mut spans = vec![Span::styled(
            clip_display(&buffer.rel, left_area.width as usize),
            style,
        )];
        if buffer.dirty {
            spans.push(Span::styled(" *", Style::default().fg(theme::AMBER)));
        }
        if buffer.external_change {
            spans.push(Span::styled(
                " !",
                Style::default()
                    .fg(theme::AMBER)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
            left_area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                clip_display(&right, right_area.width as usize),
                Style::default().fg(theme::DIM),
            )))
            .alignment(Alignment::Right)
            .style(Style::default().bg(theme::BG)),
            right_area,
        );
    }

    let tabs_height = if app.editor.buffers.len() > 1 && inner.height > 2 {
        1
    } else {
        0
    };
    if tabs_height == 1 {
        let y = inner.y + 1;
        let right = inner.x.saturating_add(inner.width);
        let mut x = inner.x;
        let mut spans = Vec::new();
        for (index, buffer) in app.editor.buffers.iter().enumerate() {
            let name = buffer
                .path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| buffer.rel.clone());
            let label = format!(" {name} ");
            let width = display_width(&label) as u16;
            if width == 0 || x.saturating_add(width) > right {
                break;
            }
            let rect = Rect {
                x,
                y,
                width,
                height: 1,
            };
            let hovered = app
                .hover
                .is_some_and(|(hx, hy)| hx >= rect.x && hx < rect.x + rect.width && hy == rect.y);
            let style = if index == app.editor.active {
                Style::default()
                    .fg(theme::ACCENT)
                    .bg(theme::SELECT)
                    .add_modifier(Modifier::BOLD)
            } else if hovered {
                Style::default().fg(theme::FG).bg(theme::HOVER)
            } else {
                Style::default().fg(theme::DIM)
            };
            spans.push(Span::styled(label, style));
            app.ui.tabs.push((rect, index));
            x = x.saturating_add(width);
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }

    let text_area = Rect {
        x: inner.x,
        y: inner.y + 1 + tabs_height,
        width: inner.width,
        height: inner.height.saturating_sub(1 + tabs_height),
    };
    app.ui.editor_text = text_area;

    let Some(line_count) = app.editor.buffer().map(|buffer| buffer.lines.len()) else {
        app.ui.gutter = 0;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "select a file",
                Style::default().fg(theme::DIM),
            )))
            .style(Style::default().bg(theme::BG)),
            text_area,
        );
        return;
    };

    if text_area.height == 0 {
        return;
    }
    let digits = line_count.max(1).to_string().len();
    let gutter = (digits + 1) as u16;
    app.ui.gutter = gutter;
    let text_width = text_area.width.saturating_sub(gutter) as usize;
    if text_width == 0 {
        return;
    }

    let focused = app.focus == Focus::Editor;
    if app.follow_editor_cursor {
        app.editor
            .ensure_visible(text_area.height as usize, text_width);
    }
    let rows = app.editor.visual_rows(text_width);
    let wrap = app.editor.wrap;

    let tracks = &app.tracks;
    let Some(buffer) = app.editor.buffer() else {
        return;
    };
    let cursor = app.editor.cursor_visual(text_width);
    let ghost = app.editor.ghost.as_deref();
    let ghost_lines: Vec<&str> = ghost
        .map(|text| text.split('\n').collect())
        .unwrap_or_default();
    let height = text_area.height as usize;

    let mut lines = Vec::with_capacity(height);
    let mut out_row = 0usize;
    let mut display_row = buffer.scroll;
    while out_row < height {
        let Some(visual) = rows.get(display_row).copied() else {
            break;
        };
        let index = visual.line;
        let number = index + 1;
        let tracked = track_at(tracks, &buffer.path, number);
        let bg = match tracked {
            Some(TrackKind::Read) => theme::READ_BG,
            Some(TrackKind::Write) => theme::WRITE_BG,
            None if index == buffer.cursor_line => theme::CURSOR_LINE,
            None => theme::BG,
        };
        let number_color = match tracked {
            Some(TrackKind::Read) => theme::ACCENT,
            Some(TrackKind::Write) => theme::AMBER,
            None => theme::FAINT,
        };
        let label = if wrap && visual.start > 0 {
            " ".repeat(digits + 1)
        } else {
            format!("{:>width$} ", number, width = digits)
        };

        let mut plain_spans = Vec::new();
        let raw = if let Some(spans) = buffer.matching_spans(index) {
            spans
        } else {
            if !buffer.lines[index].is_empty() {
                plain_spans.push((Style::default().fg(theme::FG), buffer.lines[index].clone()));
            }
            plain_spans.as_slice()
        };
        let mut spans = vec![Span::styled(
            label,
            Style::default().fg(number_color).bg(bg),
        )];
        let selection = selection_columns(buffer, index);
        let clipped = if wrap {
            wrapped_spans(raw, visual.start, visual.end, text_width, bg, selection)
        } else {
            clipped_spans(raw, buffer.hscroll, text_width, bg, selection)
        };
        let mut used: usize = clipped
            .iter()
            .map(|span| display_width(&span.content))
            .sum();
        spans.extend(clipped);

        let cursor_row = focused
            && cursor.map(|(cursor_row, _)| cursor_row == display_row) == Some(true)
            && buffer.selection.is_none();
        if cursor_row {
            if let (Some(first), Some((_, column))) = (ghost_lines.first(), cursor) {
                let start = if wrap {
                    column
                } else {
                    column.saturating_sub(buffer.hscroll)
                };
                if start < text_width {
                    if used < start {
                        spans.push(Span::styled(
                            " ".repeat(start - used),
                            Style::default().bg(bg),
                        ));
                        used = start;
                    }
                    let visible = clip_display(first, text_width.saturating_sub(start));
                    used += display_width(&visible);
                    spans.push(Span::styled(
                        visible,
                        Style::default()
                            .fg(theme::DIM)
                            .bg(bg)
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
            }
        }

        if used < text_width {
            spans.push(Span::styled(
                " ".repeat(text_width - used),
                Style::default().bg(bg),
            ));
        }
        lines.push(Line::from(spans));
        out_row += 1;

        if cursor_row && ghost_lines.len() > 1 {
            for continuation in ghost_lines.iter().skip(1) {
                if out_row >= height {
                    break;
                }
                let mut spans = vec![Span::styled(
                    " ".repeat(digits + 1),
                    Style::default().fg(theme::FAINT).bg(theme::BG),
                )];
                let visible = clip_display(continuation, text_width);
                let used = display_width(&visible);
                spans.push(Span::styled(
                    visible,
                    Style::default()
                        .fg(theme::DIM)
                        .bg(theme::BG)
                        .add_modifier(Modifier::ITALIC),
                ));
                if used < text_width {
                    spans.push(Span::styled(
                        " ".repeat(text_width - used),
                        Style::default().bg(theme::BG),
                    ));
                }
                lines.push(Line::from(spans));
                out_row += 1;
            }
        }

        display_row += 1;
    }

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::BG)),
        text_area,
    );

    if focused {
        if let Some((row, column)) = cursor {
            if row >= buffer.scroll && row < buffer.scroll + height {
                let column = if wrap {
                    column
                } else {
                    column.saturating_sub(buffer.hscroll)
                };
                let x = text_area.x as usize + gutter as usize + column;
                let y = text_area.y + (row - buffer.scroll) as u16;
                if x < (text_area.x + text_area.width) as usize {
                    frame.set_cursor_position(Position::new(x as u16, y));
                }
            }
        }
    }
}

fn draw_unsupported(frame: &mut Frame, inner: Rect, header: Rect, app: &mut App, path: &Path) {
    app.ui.editor_text = Rect::default();
    app.ui.gutter = 0;
    let rel = app.editor.rel_of(path);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            clip_display(&rel, header.width as usize),
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(theme::BG)),
        header,
    );

    let body = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if body.width == 0 || body.height < 4 {
        return;
    }

    let message_y = body.y + body.height / 2 - 1;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "this file type is not supported.",
            Style::default().fg(theme::DIM),
        )))
        .alignment(Alignment::Center)
        .style(Style::default().bg(theme::BG)),
        Rect {
            x: body.x,
            y: message_y,
            width: body.width,
            height: 1,
        },
    );

    let label = " open in default app ";
    let width = display_width(label) as u16;
    if width > body.width {
        return;
    }
    let button = Rect {
        x: body.x + (body.width - width) / 2,
        y: message_y + 2,
        width,
        height: 1,
    };
    if button.y >= body.y + body.height {
        return;
    }
    app.ui.unsupported_button = button;
    let hovered = app.hover.is_some_and(|(x, y)| {
        x >= button.x
            && x < button.x + button.width
            && y >= button.y
            && y < button.y + button.height
    });
    let style = if hovered {
        Style::default().fg(theme::FG).bg(theme::SELECT)
    } else {
        Style::default().fg(theme::ACCENT).bg(theme::PANEL_ALT)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label, style)))
            .style(Style::default().bg(theme::BG)),
        button,
    );
}

fn working_label(elapsed_ms: u128) -> &'static str {
    const FRAMES: [&str; 4] = ["working", "working.", "working..", "working..."];
    FRAMES[(elapsed_ms / 280) as usize % FRAMES.len()]
}

/// Most task rows the todo panel may show; the rest are implied by the
/// `done/total` heading.
const TODO_MAX_ROWS: usize = 8;

/// Rows the todo panel takes from the agent pane: one heading plus tasks,
/// never leaving fewer than three rows for the transcript.
fn todo_panel_height(app: &App, free: u16) -> u16 {
    if app.todo.is_empty() {
        return 0;
    }
    let budget = free.saturating_sub(3);
    if budget < 3 {
        return 0;
    }
    let rows = 1 + app.todo.tasks.len().min(TODO_MAX_ROWS) as u16;
    rows.min(budget)
}

fn draw_todo_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let tasks = &app.todo.tasks;
    let done = app.todo.done();
    let total = tasks.len();

    let heading = format!(" todos {done}/{total} ");
    let percent = if total == 0 {
        0
    } else {
        done * 100 / total
    };
    let mut right = String::new();
    if width >= display_width(&heading) + 16 {
        let inner = width
            .saturating_sub(display_width(&heading) + 8)
            .clamp(4, 14);
        let filled = (done * inner + total / 2) / total.max(1);
        right = format!(
            "[{}{}] {}%",
            "#".repeat(filled),
            "-".repeat(inner - filled),
            percent
        );
    }
    let gap = width.saturating_sub(display_width(&heading) + display_width(&right));
    let head_bg = Style::default().bg(theme::PANEL_ALT);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            heading,
            head_bg
                .fg(theme::FG)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ".repeat(gap), head_bg),
        Span::styled(
            right,
            head_bg.fg(if done == total {
                theme::GREEN
            } else {
                theme::DIM
            }),
        ),
    ])];

    let visible = area.height.saturating_sub(1) as usize;
    let mut rows: Vec<(usize, &TodoTask)> = tasks.iter().enumerate().take(visible).collect();
    if let Some(active) = app.todo.active_index() {
        // The running task is the one worth seeing when the list overflows.
        if active >= visible && visible > 0 {
            rows.pop();
            rows.push((active, &tasks[active]));
        }
    }

    for (_index, task) in rows {
        let (marker, color) = match task.status {
            TodoStatus::Pending => ("[ ]", theme::FAINT),
            TodoStatus::Active => ("[>]", theme::ACCENT),
            TodoStatus::Done => ("[x]", theme::GREEN),
        };
        let time = app
            .todo
            .elapsed(&task.title, app.now)
            .map(format_duration)
            .unwrap_or_default();
        let time_width = display_width(&time);
        let title_width = width.saturating_sub(4 + time_width + 1);
        let title = clip_display(&task.title, title_width);
        let title_width = display_width(&title);
        let gap = width.saturating_sub(4 + title_width + time_width);

        let mut title_style = Style::default().fg(if task.status == TodoStatus::Active {
            theme::FG
        } else {
            theme::DIM
        });
        if task.status == TodoStatus::Done {
            title_style = title_style.add_modifier(Modifier::CROSSED_OUT);
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} "),
                Style::default().fg(color),
            ),
            Span::styled(title, title_style),
            Span::styled(" ".repeat(gap), title_style),
            Span::styled(time, Style::default().fg(theme::FAINT)),
        ]));
    }

    while lines.len() < area.height as usize {
        lines.push(Line::from(""));
    }
    lines.truncate(area.height as usize);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        area,
    );
}

fn draw_agent(frame: &mut Frame, rect: Rect, app: &mut App) {
    let border = if matches!(app.resizing, Some(ResizeEdge::Agent)) {
        theme::ACCENT
    } else if app.hover_edge == Some(ResizeEdge::Agent) {
        theme::DIM
    } else {
        theme::BORDER
    };
    let block = Block::default()
        .style(Style::default().bg(theme::PANEL))
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(border));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let content = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    app.ui.agent = content;
    app.ui.agent_input = Rect::default();
    if content.width == 0 || content.height == 0 {
        return;
    }

    let (status, status_color) = match &app.pi {
        PiState::Missing => ("not installed", theme::AMBER),
        PiState::Failed(_) => ("failed", theme::RED),
        PiState::Running(_) if app.agent.busy => (
            working_label(app.now.duration_since(app.started_at).as_millis()),
            theme::ACCENT,
        ),
        PiState::Running(_) => (app.model.as_deref().unwrap_or("idle"), theme::DIM),
    };
    let title_style = if app.focus == Focus::Agent {
        Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme::FAINT)
            .add_modifier(Modifier::BOLD)
    };
    let status_width = display_width(status).min(content.width as usize / 2) as u16;
    let [title_area, status_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(status_width)]).areas(Rect {
            x: content.x,
            y: content.y,
            width: content.width,
            height: 1,
        });
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("agent", title_style)))
            .style(Style::default().bg(theme::PANEL)),
        title_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            clip_display(status, status_area.width as usize),
            Style::default().fg(status_color),
        )))
        .alignment(Alignment::Right)
        .style(Style::default().bg(theme::PANEL)),
        status_area,
    );

    let running = matches!(app.pi, PiState::Running(_));
    let input_height = if running && content.height >= 2 { 1 } else { 0 };
    let free = content
        .height
        .saturating_sub(1 + input_height);
    let todo_height = todo_panel_height(app, free);
    let body = Rect {
        x: content.x,
        y: content.y + 1,
        width: content.width,
        height: free.saturating_sub(todo_height),
    };

    app.ui.agent_thinking.clear();
    match &app.pi {
        PiState::Missing if app.agent.entries.is_empty() => draw_missing(frame, body),
        PiState::Failed(error) if app.agent.entries.is_empty() => draw_failed(frame, body, error),
        _ => draw_conversation(frame, body, app),
    }

    if todo_height > 0 {
        let panel = Rect {
            x: content.x,
            y: content.y + content.height - input_height - todo_height,
            width: content.width,
            height: todo_height,
        };
        draw_todo_panel(frame, panel, app);
    }

    if input_height == 1 {
        let input = Rect {
            x: content.x,
            y: content.y + content.height - 1,
            width: content.width,
            height: 1,
        };
        app.ui.agent_input = input;
        draw_agent_input(frame, input, app);
    }
}

fn draw_missing(frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(
            "pi not installed",
            Style::default()
                .fg(theme::AMBER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "https://pi.dev",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::UNDERLINED),
        )),
        Line::from(""),
    ];
    for part in wrap_text(install_command(), width) {
        lines.push(Line::from(Span::styled(
            part,
            Style::default().fg(theme::DIM),
        )));
    }
    lines.push(Line::from(""));
    for part in wrap_text("restart smol after installing", width) {
        lines.push(Line::from(Span::styled(
            part,
            Style::default().fg(theme::FAINT),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        area,
    );
}

fn draw_failed(frame: &mut Frame, area: Rect, error: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;
    let mut lines = vec![
        Line::from(Span::styled(
            "pi failed",
            Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for part in wrap_text(error, width) {
        lines.push(Line::from(Span::styled(
            part,
            Style::default().fg(theme::DIM),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "https://pi.dev",
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::UNDERLINED),
    )));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        area,
    );
}

fn draw_conversation(frame: &mut Frame, area: Rect, app: &mut App) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let width = area.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut thinking_rows: Vec<(usize, usize)> = Vec::new();

    for (entry_index, entry) in app.agent.entries.iter().enumerate() {
        match entry {
            Entry::User(text) => {
                if !text.trim().is_empty() {
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        "you",
                        Style::default()
                            .fg(theme::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    )));
                    lines.extend(render_prompt(text, width));
                }
            }
            Entry::Assistant {
                text,
                thinking,
                thinking_open,
            } => {
                let has_thinking = !thinking.trim().is_empty();
                let has_text = !text.trim().is_empty();
                if has_thinking || has_text {
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    if has_thinking {
                        thinking_rows.push((lines.len(), entry_index));
                        let marker = if *thinking_open {
                            "\u{25be}"
                        } else {
                            "\u{25b8}"
                        };
                        lines.push(Line::from(Span::styled(
                            format!("{marker} thinking"),
                            Style::default()
                                .fg(theme::DIM)
                                .add_modifier(Modifier::ITALIC),
                        )));
                        if *thinking_open {
                            for part in wrap_text(thinking.trim_end(), width) {
                                lines.push(Line::from(Span::styled(
                                    part,
                                    Style::default()
                                        .fg(theme::FAINT)
                                        .add_modifier(Modifier::ITALIC),
                                )));
                            }
                        }
                        if has_text {
                            lines.push(Line::from(""));
                        }
                    }
                    if has_text {
                        lines.extend(crate::markdown::render(text, width));
                    }
                }
            }
            Entry::Tool(tool) => {
                let color = match tool.state {
                    ToolState::Running => theme::ACCENT,
                    ToolState::Done => theme::DIM,
                    ToolState::Failed => theme::RED,
                };
                let mut label = tool.label.clone();
                if tool.state == ToolState::Running {
                    let frames = [".  ", ".. ", "...", " .."];
                    let frame = (app.now.duration_since(app.started_at).as_millis() / 180) as usize
                        % frames.len();
                    label.push(' ');
                    label.push_str(frames[frame]);
                }
                for part in wrap_text(&label, width) {
                    lines.push(Line::from(Span::styled(part, Style::default().fg(color))));
                }
                if !tool.body.is_empty() && width > 2 {
                    for body in &tool.body {
                        let body_color = match body.kind {
                            ToolBodyKind::Text => theme::FG,
                            ToolBodyKind::Add => theme::GREEN,
                            ToolBodyKind::Del => theme::RED,
                            ToolBodyKind::Meta => theme::FAINT,
                        };
                        for part in wrap_text(&body.text, width - 2) {
                            lines.push(Line::from(Span::styled(
                                format!("  {part}"),
                                Style::default().fg(body_color),
                            )));
                        }
                    }
                }
            }
            Entry::Note(note) => {
                for part in wrap_text(note, width) {
                    lines.push(Line::from(Span::styled(
                        part,
                        Style::default().fg(theme::AMBER),
                    )));
                }
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "ask pi about this project",
            Style::default().fg(theme::FAINT),
        )));
    }

    let height = area.height as usize;
    let max_scroll = lines.len().saturating_sub(height);
    app.agent.scroll = app.agent.scroll.min(max_scroll);
    let start = lines
        .len()
        .saturating_sub(height)
        .saturating_sub(app.agent.scroll);
    let end = (start + height).min(lines.len());
    let visible: Vec<Line<'static>> = lines[start..end].to_vec();
    for (row, entry_index) in thinking_rows {
        if row >= start && row < end {
            app.ui.agent_thinking.push((
                Rect {
                    x: area.x,
                    y: area.y + (row - start) as u16,
                    width: area.width,
                    height: 1,
                },
                entry_index,
            ));
        }
    }

    frame.render_widget(
        Paragraph::new(visible).style(Style::default().bg(theme::PANEL)),
        area,
    );
}

fn render_prompt(text: &str, width: usize) -> Vec<Line<'static>> {
    let source: Vec<&str> = text.lines().collect();
    let mut lines = Vec::new();
    let mut plain_start = 0;
    let mut index = 0;

    while index < source.len() {
        let Some(name) = opening_tag(source[index]) else {
            index += 1;
            continue;
        };
        let Some(close) = (index + 1..source.len())
            .find(|candidate| closing_tag(source[*candidate]) == Some(name))
        else {
            index += 1;
            continue;
        };

        push_prompt_section(
            &mut lines,
            crate::markdown::render(&source[plain_start..index].join("\n"), width),
        );
        let mut block = vec![Line::from(Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme::DIM)
                .add_modifier(Modifier::ITALIC),
        ))];
        block.extend(crate::markdown::code_block(
            &source[index + 1..close].join("\n"),
            width,
        ));
        push_prompt_section(&mut lines, block);

        index = close + 1;
        plain_start = index;
    }

    if plain_start < source.len() {
        push_prompt_section(
            &mut lines,
            crate::markdown::render(&source[plain_start..].join("\n"), width),
        );
    }

    lines
}

fn push_prompt_section(lines: &mut Vec<Line<'static>>, section: Vec<Line<'static>>) {
    if section
        .iter()
        .all(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
    {
        return;
    }
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines.extend(section);
}

fn opening_tag(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix('<')?.strip_suffix('>')?;
    let name = inner.split_whitespace().next()?;
    valid_tag_name(name).then_some(name)
}

fn closing_tag(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix("</")?.strip_suffix('>')?.trim();
    valid_tag_name(inner).then_some(inner)
}

fn valid_tag_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn draw_agent_input(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Agent;
    let hovered = app.hover.is_some_and(|(x, y)| {
        x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
    });
    let prompt_style = if focused || hovered {
        Style::default().fg(theme::ACCENT)
    } else {
        Style::default().fg(theme::FAINT)
    };
    let mut spans = vec![Span::styled("> ", prompt_style)];
    let width = area.width as usize;
    let available = width.saturating_sub(2);

    if app.agent.input.is_empty() {
        spans.push(Span::styled(
            "ask pi",
            Style::default()
                .fg(theme::FAINT)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let chars: Vec<char> = app.agent.input.chars().collect();
        let prefix: String = chars[..app.agent.input_cursor.min(chars.len())]
            .iter()
            .collect();
        let prefix_width = display_width(&prefix);
        let start = prefix_width.saturating_sub(available.saturating_sub(1));
        let visible = clip_from(&app.agent.input, start, available);
        spans.push(Span::styled(visible, Style::default().fg(theme::FG)));
        if focused {
            let x = area.x as usize + 2 + prefix_width.saturating_sub(start);
            if x < (area.x + area.width) as usize {
                frame.set_cursor_position(Position::new(x as u16, area.y));
            }
        }
    }

    if focused && app.agent.input.is_empty() {
        frame.set_cursor_position(Position::new(area.x + 2, area.y));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::PANEL)),
        area,
    );

    if focused && !app.agent.suggestions.is_empty() {
        draw_agent_suggestions(frame, area, app);
    }
}

fn draw_agent_suggestions(frame: &mut Frame, input: Rect, app: &App) {
    let top = app.ui.agent.y.saturating_add(1);
    let available = input.y.saturating_sub(top) as usize;
    if input.width < 4 || available < 3 {
        return;
    }
    let rows = app
        .agent
        .suggestions
        .len()
        .min(8)
        .min(available.saturating_sub(2));
    if rows == 0 {
        return;
    }

    let height = rows as u16 + 2;
    let popup = Rect {
        x: input.x,
        y: input.y.saturating_sub(height),
        width: input.width,
        height,
    };
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" files ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(popup);
    let start = app
        .agent
        .suggestion_selected
        .saturating_sub(rows.saturating_sub(1));
    let lines = app
        .agent
        .suggestions
        .iter()
        .skip(start)
        .take(rows)
        .enumerate()
        .map(|(offset, path)| {
            let selected = start + offset == app.agent.suggestion_selected;
            let style = if selected {
                Style::default().fg(theme::FG).bg(theme::SELECT)
            } else {
                Style::default().fg(theme::DIM).bg(theme::PANEL)
            };
            Line::from(Span::styled(format!(" @{path}"), style))
        })
        .collect::<Vec<_>>();

    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        inner,
    );
}

fn draw_diff(frame: &mut Frame, area: Rect, app: &mut App) {
    app.ui.gutter = 0;
    app.ui.editor_text = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    let Some(diff) = &mut app.diff else {
        return;
    };
    let header = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    app.ui.diff_header = header;
    let hint = "esc close";
    let hint_width = display_width(hint) as u16;
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_width)]).areas(header);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "diff",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default().fg(theme::FAINT)),
            Span::styled(
                clip_display(&diff.title, left.width.saturating_sub(4) as usize),
                Style::default().fg(theme::DIM),
            ),
        ]))
        .style(Style::default().bg(theme::BG)),
        left,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme::FAINT),
        )))
        .alignment(Alignment::Right)
        .style(Style::default().bg(theme::BG)),
        right,
    );

    let body = app.ui.editor_text;
    if body.width == 0 || body.height == 0 {
        return;
    }
    let height = body.height as usize;
    let max_scroll = diff.lines.len().saturating_sub(height);
    diff.scroll = diff.scroll.min(max_scroll);
    let mut lines = Vec::new();
    for line in diff.lines.iter().skip(diff.scroll).take(height) {
        let color = match line.kind {
            DiffKind::Add => theme::GREEN,
            DiffKind::Del => theme::RED,
            DiffKind::Hunk => theme::ACCENT,
            DiffKind::Meta => theme::DIM,
            DiffKind::Context => theme::FG,
        };
        lines.push(Line::from(Span::styled(
            clip_display(&line.text, body.width as usize),
            Style::default().fg(color),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no diff",
            Style::default().fg(theme::FAINT),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::BG)),
        body,
    );
}

fn draw_menu(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(menu) = &app.menu else {
        return;
    };
    let Some(page) = menu.pages.last() else {
        return;
    };
    let width = area.width.saturating_sub(8).clamp(32, 64).min(area.width);
    let height = (page.items.len() as u16 + 2).min(area.height);
    if width == 0 || height == 0 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let title = if menu.pages.len() > 1 {
        format!(" commands / {} ", page.title)
    } else {
        format!(" {} ", page.title)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(title)
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines = Vec::new();
    for (index, item) in page.items.iter().enumerate().take(inner.height as usize) {
        let selected = index == page.selected;
        let bg = if selected {
            theme::SELECT
        } else {
            theme::PANEL
        };
        let label_style = Style::default()
            .fg(if selected { theme::FG } else { theme::DIM })
            .bg(bg);
        let hint_style = Style::default()
            .fg(if selected { theme::FG } else { theme::FAINT })
            .bg(bg);
        let hint = clip_display(&item.hint, width / 2);
        let hint_width = display_width(&hint);
        let label = clip_display(&item.label, width.saturating_sub(hint_width + 2));
        let label_width = display_width(&label);
        let used = label_width + hint_width;
        let gap = width.saturating_sub(used);

        let mut spans = vec![Span::styled(label, label_style)];
        if !hint.is_empty() {
            spans.push(Span::styled(" ".repeat(gap), label_style));
            spans.push(Span::styled(hint, hint_style));
        } else if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), label_style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        inner,
    );
}

fn draw_context_menu(frame: &mut Frame, area: Rect, app: &mut App) {
    app.ui.context_menu = Rect::default();
    let Some(menu) = &app.context_menu else {
        return;
    };
    let label_width = menu
        .items
        .iter()
        .map(|item| display_width(item.label))
        .max()
        .unwrap_or(0);
    let hint_width = menu
        .items
        .iter()
        .map(|item| display_width(item.hint))
        .max()
        .unwrap_or(0);
    let width = (label_width + hint_width + 4).clamp(24, 48) as u16;
    let width = width.min(area.width);
    let height = (menu.items.len() as u16 + 2).min(area.height);
    if width == 0 || height == 0 {
        return;
    }
    let max_x = area.x.saturating_add(area.width.saturating_sub(width));
    let max_y = area.y.saturating_add(area.height.saturating_sub(height));
    let rect = Rect {
        x: menu.anchor.0.min(max_x).max(area.x),
        y: menu.anchor.1.min(max_y).max(area.y),
        width,
        height,
    };
    app.ui.context_menu = rect;
    frame.render_widget(Clear, rect);

    let name = menu
        .target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| menu.target.display().to_string());
    let title = format!(
        " {} ",
        clip_display(&name, width.saturating_sub(4) as usize)
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(title)
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines = Vec::new();
    for (index, item) in menu.items.iter().enumerate().take(inner.height as usize) {
        let selected = index == menu.selected;
        let bg = if selected {
            theme::SELECT
        } else {
            theme::PANEL
        };
        let label_style = Style::default()
            .fg(if selected { theme::FG } else { theme::DIM })
            .bg(bg);
        let hint_style = Style::default()
            .fg(if selected { theme::FG } else { theme::FAINT })
            .bg(bg);
        let hint = clip_display(item.hint, width / 2);
        let hint_width = display_width(&hint);
        let label = clip_display(item.label, width.saturating_sub(hint_width + 2));
        let label_width = display_width(&label);
        let gap = width.saturating_sub(label_width + hint_width);
        let mut spans = vec![Span::styled(label, label_style)];
        if !hint.is_empty() {
            spans.push(Span::styled(" ".repeat(gap), label_style));
            spans.push(Span::styled(hint, hint_style));
        } else if gap > 0 {
            spans.push(Span::styled(" ".repeat(gap), label_style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        inner,
    );
}

fn draw_session_history(frame: &mut Frame, area: Rect, app: &mut App) {
    app.ui.history = Rect::default();
    app.ui.history_rows.clear();
    let Some(history) = &app.history else {
        return;
    };
    let width = area.width.saturating_sub(8).clamp(48, 88).min(area.width);
    let desired = (history.sessions.len() as u16).saturating_add(3);
    let height = desired.clamp(7, 22).min(area.height);
    if width == 0 || height == 0 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);

    let title = format!(
        " session history · {} ",
        history.sessions.len()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(clip_display(&title, width.saturating_sub(4) as usize))
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    app.ui.history = inner;

    let width = inner.width as usize;
    let list_height = inner.height.saturating_sub(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    if history.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "no saved sessions yet",
            Style::default().fg(theme::FAINT),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "send a prompt to start one",
            Style::default().fg(theme::FAINT),
        )));
    } else {
        let offset = history
            .selected
            .saturating_sub(list_height.saturating_sub(1));
        for (index, session) in history
            .sessions
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_height)
        {
            let selected = index == history.selected;
            let active = app.session_path.as_ref() == Some(&session.path);
            let bg = if selected {
                theme::SELECT
            } else {
                theme::PANEL
            };
            let prefix = format!(
                "{}{} ",
                if selected { '>' } else { ' ' },
                if active { '*' } else { ' ' }
            );
            let message_word = if session.message_count == 1 {
                "msg"
            } else {
                "msgs"
            };
            let hint = format!("{}  {} {}", session.age(), session.message_count, message_word);
            let hint_width = display_width(&hint);
            let label = clip_display(
                &session.label(),
                width.saturating_sub(display_width(&prefix) + hint_width + 3),
            );
            let label_width = display_width(&label);
            let used = display_width(&prefix) + label_width + hint_width + 1;
            let gap = width.saturating_sub(used);

            let label_style = Style::default()
                .fg(if selected { theme::FG } else { theme::DIM })
                .bg(bg);
            let hint_style = Style::default()
                .fg(if selected { theme::FG } else { theme::FAINT })
                .bg(bg);
            let mut spans = vec![Span::styled(
                format!("{prefix}{label}"),
                label_style,
            )];
            spans.push(Span::styled(" ".repeat(gap), label_style));
            spans.push(Span::styled(hint, hint_style));
            let row_y = inner.y + (lines.len() as u16);
            lines.push(Line::from(spans));
            if row_y < inner.y + inner.height {
                app.ui.history_rows.push((
                    Rect {
                        x: inner.x,
                        y: row_y,
                        width: inner.width,
                        height: 1,
                    },
                    index,
                ));
            }
        }
    }

    while lines.len() < list_height {
        lines.push(Line::from(""));
    }
    lines.truncate(list_height);

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: list_height as u16,
        },
    );

    let footer = if let Some(path) = &history.confirm {
        let name = history
            .sessions
            .iter()
            .find(|session| &session.path == path)
            .map(|session| session.label())
            .unwrap_or_default();
        format!(
            "press d or enter to delete {}  esc cancel",
            clip_display(&name, width.saturating_sub(40))
        )
    } else if history.sessions.is_empty() {
        "esc close".to_string()
    } else {
        "enter resume  d delete  r refresh  esc close".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            clip_display(&footer, width),
            Style::default().fg(if history.confirm.is_some() {
                theme::AMBER
            } else {
                theme::FAINT
            }),
        )))
        .style(Style::default().bg(theme::PANEL)),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
}

fn draw_picker(frame: &mut Frame, area: Rect, app: &mut App) {
    app.ui.picker = Rect::default();
    app.ui.picker_rows.clear();
    let Some(picker) = &app.picker else {
        return;
    };
    let width = area.width.saturating_sub(8).clamp(52, 84).min(area.width);
    let desired = (picker.items.len() as u16).saturating_add(3);
    let height = desired.clamp(7, 24).min(area.height);
    if width == 0 || height == 0 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);

    let title = format!(" {} ", picker.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT))
        .title(clip_display(&title, width.saturating_sub(4) as usize))
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    app.ui.picker = inner;

    let width = inner.width as usize;
    let list_height = inner.height.saturating_sub(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    if picker.items.is_empty() {
        let (first, second) = if matches!(app.pi, PiState::Running(_)) {
            ("loading models", "one moment")
        } else {
            ("no models available", "check pi's model config")
        };
        lines.push(Line::from(Span::styled(
            first,
            Style::default().fg(theme::FAINT),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            second,
            Style::default().fg(theme::FAINT),
        )));
    } else {
        let offset = picker
            .selected
            .saturating_sub(list_height.saturating_sub(1));
        for (index, item) in picker
            .items
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_height)
        {
            let selected = index == picker.selected;
            let active = item.active;
            let bg = if selected {
                theme::SELECT
            } else {
                theme::PANEL
            };
            let prefix = format!(
                " {}{} ",
                if selected { '>' } else { ' ' },
                if active { '*' } else { ' ' }
            );
            let hint = item.hint.clone();
            let hint_width = display_width(&hint);
            let label = clip_display(
                &item.label,
                width.saturating_sub(display_width(&prefix) + hint_width + 3),
            );
            let label_width = display_width(&label);
            let used = display_width(&prefix) + label_width + hint_width + 1;
            let gap = width.saturating_sub(used);

            let label_style = Style::default()
                .fg(if selected { theme::FG } else { theme::DIM })
                .bg(bg);
            let hint_style = Style::default()
                .fg(if selected { theme::FG } else { theme::FAINT })
                .bg(bg);
            let mut spans = vec![Span::styled(format!("{prefix}{label}"), label_style)];
            spans.push(Span::styled(" ".repeat(gap), label_style));
            spans.push(Span::styled(hint, hint_style));
            let row_y = inner.y + (lines.len() as u16);
            lines.push(Line::from(spans));
            if row_y < inner.y + inner.height {
                app.ui.picker_rows.push((
                    Rect {
                        x: inner.x,
                        y: row_y,
                        width: inner.width,
                        height: 1,
                    },
                    index,
                ));
            }
        }
    }

    while lines.len() < list_height {
        lines.push(Line::from(""));
    }
    lines.truncate(list_height);

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: list_height as u16,
        },
    );

    let footer = if picker.items.is_empty() {
        "esc close".to_string()
    } else {
        picker.footer.clone()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            clip_display(&footer, width),
            Style::default().fg(theme::FAINT),
        )))
        .style(Style::default().bg(theme::PANEL)),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
}

fn draw_file_dialog(frame: &mut Frame, area: Rect, app: &mut App) {
    let Some(dialog) = &app.dialog else {
        return;
    };
    let width = area.width.saturating_sub(8).clamp(36, 64).min(area.width);
    let height = match dialog {
        FileDialog::Delete { .. } => 8,
        FileDialog::Rename { .. } | FileDialog::Create { .. } => 7,
        FileDialog::Provider { fields, .. } => fields.len() as u16 + 4,
    }
    .min(area.height);
    if width == 0 || height == 0 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);

    let (title, border) = match dialog {
        FileDialog::Delete { .. } => (" delete ".to_string(), theme::RED),
        FileDialog::Rename { prompt, .. } | FileDialog::Create { prompt, .. } => {
            (format!(" {} ", prompt.title), theme::ACCENT)
        }
        FileDialog::Provider { .. } => (" connect provider ".to_string(), theme::ACCENT),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(clip_display(&title, width.saturating_sub(4) as usize))
        .style(Style::default().bg(theme::PANEL));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let mut lines = Vec::new();
    let mut cursor = None;
    match dialog {
        FileDialog::Rename { prompt, .. } | FileDialog::Create { prompt, .. } => {
            lines.push(Line::from(Span::styled(
                "name",
                Style::default().fg(theme::FAINT),
            )));
            lines.push(Line::default());
            let shown = clip_display(&prompt.value, width);
            let shown_width = display_width(&shown);
            let mut spans = vec![Span::styled(
                shown,
                Style::default().fg(theme::FG).bg(theme::SELECT),
            )];
            if shown_width < width {
                spans.push(Span::styled(
                    " ".repeat(width - shown_width),
                    Style::default().bg(theme::SELECT),
                ));
            }
            lines.push(Line::from(spans));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "enter confirm  esc cancel",
                Style::default().fg(theme::FAINT),
            )));
            let before: String = prompt.value.chars().take(prompt.cursor).collect();
            let x = inner.x + (display_width(&before) as u16).min(inner.width.saturating_sub(1));
            cursor = Some(Position::new(x, inner.y + 2));
        }
        FileDialog::Delete {
            target,
            is_dir,
            dirty,
        } => {
            let name = target
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| target.display().to_string());
            lines.push(Line::from(Span::styled(
                format!("Delete {name}?"),
                Style::default().fg(theme::FG),
            )));
            lines.push(Line::from(Span::styled(
                if *is_dir {
                    "The folder and all its contents will be removed."
                } else {
                    "The file will be removed."
                },
                Style::default().fg(theme::RED),
            )));
            lines.push(Line::from(Span::styled(
                if *dirty {
                    "Unsaved changes will be lost."
                } else {
                    "This cannot be undone."
                },
                Style::default().fg(theme::AMBER),
            )));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "enter delete  esc cancel",
                Style::default().fg(theme::FAINT),
            )));
        }
        FileDialog::Provider { fields, selected } => {
            let label_col = 17usize.min(width.saturating_sub(4));
            let pad = label_col.saturating_sub(2);
            for (index, field) in fields.iter().enumerate() {
                let active = index == *selected;
                let bg = if active {
                    theme::SELECT
                } else {
                    theme::PANEL
                };
                let label = clip_display(&field.title, pad);
                let value_width = width.saturating_sub(label_col);
                let shown = clip_display(&field.value, value_width);
                let gap = width.saturating_sub(label_col + display_width(&shown));
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {label:<pad$} "),
                        Style::default()
                            .fg(if active { theme::FG } else { theme::FAINT })
                            .bg(bg),
                    ),
                    Span::styled(
                        shown,
                        Style::default()
                            .fg(if active { theme::FG } else { theme::DIM })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(gap), Style::default().bg(bg)),
                ]));
                if active {
                    let before: String = field.value.chars().take(field.cursor).collect();
                    let x = inner.x
                        + (label_col as u16)
                        + (display_width(&before) as u16)
                            .min(inner.width.saturating_sub(label_col as u16 + 1));
                    cursor = Some(Position::new(x, inner.y + lines.len() as u16 - 1));
                }
            }
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "enter save  esc cancel  up/down field",
                Style::default().fg(theme::FAINT),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::PANEL)),
        inner,
    );
    if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
}

fn draw_status(frame: &mut Frame, area: Rect, app: &mut App) {
    frame.render_widget(
        Block::default().style(Style::default().bg(theme::PANEL_ALT)),
        area,
    );

    let hint = app.status_hint(area.width);
    let max_hint = (area.width as usize * 3 / 4)
        .max(24)
        .min(area.width as usize);
    let hint = clip_display(&hint, max_hint);
    let hint_width = (display_width(&hint) as u16).min(area.width);
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_width)]).areas(area);

    let mut spans = Vec::new();
    if let Some(buffer) = app.editor.buffer() {
        spans.push(Span::styled(
            clip_display(&buffer.rel, left.width as usize / 2),
            Style::default().fg(theme::FG),
        ));
        spans.push(Span::styled(
            format!("  {}:{}", buffer.cursor_line + 1, buffer.cursor_col + 1),
            Style::default().fg(theme::DIM),
        ));
    } else {
        spans.push(Span::styled(
            app.tree
                .nodes
                .first()
                .map(|node| node.name.clone())
                .unwrap_or_default(),
            Style::default().fg(theme::FG),
        ));
    }
    if let Some((message, _)) = &app.status {
        spans.push(Span::styled("  ", Style::default().fg(theme::FAINT)));
        spans.push(Span::styled(
            clip_display(message, left.width as usize / 2),
            Style::default().fg(theme::AMBER),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::PANEL_ALT)),
        left,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme::FAINT),
        )))
        .alignment(Alignment::Right)
        .style(Style::default().bg(theme::PANEL_ALT)),
        right,
    );
}

fn track_at(tracks: &[Track], path: &Path, line: usize) -> Option<TrackKind> {
    tracks
        .iter()
        .rev()
        .find(|track| track.path == path && line >= track.start && line <= track.end)
        .map(|track| track.kind)
}

fn status_color(label: &str) -> Color {
    if label.contains('?') || label.contains('A') {
        theme::GREEN
    } else if label.contains('U') || label.contains('D') {
        theme::RED
    } else {
        theme::AMBER
    }
}

fn selection_columns(buffer: &Buffer, line: usize) -> Option<(usize, usize)> {
    let anchor = buffer.selection?;
    let cursor = (buffer.cursor_line, buffer.cursor_col);
    let (start, end) = if anchor < cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    if line < start.0 || line > end.0 {
        return None;
    }
    let length = buffer
        .lines
        .get(line)
        .map(|line| line.chars().count())
        .unwrap_or(0);
    let start_col = if line == start.0 {
        start.1.min(length)
    } else {
        0
    };
    let end_col = if line == end.0 {
        end.1.min(length)
    } else {
        length
    };
    if start_col == end_col {
        None
    } else {
        Some((start_col, end_col))
    }
}

fn wrapped_spans(
    raw: &[(Style, String)],
    start: usize,
    end: usize,
    width: usize,
    bg: Color,
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let mut pieces: Vec<(Style, String)> = Vec::new();
    let mut index = 0usize;
    let mut column = 0usize;

    'outer: for (style, text) in raw {
        for ch in text.chars() {
            if index >= end {
                break 'outer;
            }
            let selected = selection
                .map(|(start, end)| index >= start && index < end)
                .unwrap_or(false);
            let piece_style = style.bg(if selected { theme::SELECTION } else { bg });
            index += 1;
            if index <= start {
                continue;
            }

            if ch == '\t' {
                let next = (column / 4 + 1) * 4;
                while column < next && column < width {
                    push_piece(piece_style, ' ', &mut pieces);
                    column += 1;
                }
                continue;
            }
            let char_width = ch.width().unwrap_or(0);
            if char_width == 0 {
                continue;
            }
            if column + char_width > width {
                break 'outer;
            }
            push_piece(piece_style, ch, &mut pieces);
            column += char_width;
        }
    }

    pieces
        .into_iter()
        .map(|(style, text)| Span::styled(text, style))
        .collect()
}

fn clipped_spans(
    raw: &[(Style, String)],
    hscroll: usize,
    width: usize,
    bg: Color,
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let mut pieces: Vec<(Style, String)> = Vec::new();
    let mut column = 0usize;
    let mut index = 0usize;

    'outer: for (style, text) in raw {
        for ch in text.chars() {
            let selected = selection
                .map(|(start, end)| index >= start && index < end)
                .unwrap_or(false);
            let piece_style = style.bg(if selected { theme::SELECTION } else { bg });
            index += 1;

            if ch == '\t' {
                let next = (column / 4 + 1) * 4;
                while column < next {
                    if column >= hscroll && column - hscroll < width {
                        push_piece(piece_style, ' ', &mut pieces);
                    }
                    column += 1;
                }
                continue;
            }
            let char_width = ch.width().unwrap_or(0);
            if char_width == 0 {
                continue;
            }
            let start = column;
            column += char_width;
            if column > hscroll + width {
                break 'outer;
            }
            if start >= hscroll && start - hscroll + char_width <= width {
                push_piece(piece_style, ch, &mut pieces);
            }
        }
    }

    pieces
        .into_iter()
        .map(|(style, text)| Span::styled(text, style))
        .collect()
}

fn push_piece(style: Style, ch: char, pieces: &mut Vec<(Style, String)>) {
    if let Some((last_style, text)) = pieces.last_mut() {
        if *last_style == style {
            text.push(ch);
            return;
        }
    }
    pieces.push((style, ch.to_string()));
}

fn clip_display(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let char_width = if ch == '\t' {
            4
        } else {
            ch.width().unwrap_or(0)
        };
        if used + char_width > width {
            break;
        }
        out.push(ch);
        used += char_width;
    }
    out
}

fn clip_from(text: &str, start: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut column = 0;
    let mut used = 0;
    for ch in text.chars() {
        let char_width = if ch == '\t' {
            4
        } else {
            ch.width().unwrap_or(0)
        };
        let next = column + char_width;
        if next <= start {
            column = next;
            continue;
        }
        if column < start {
            column = next;
            continue;
        }
        if used + char_width > width {
            break;
        }
        out.push(ch);
        used += char_width;
        column = next;
    }
    out
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let text = text.replace('\t', "    ");
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for ch in raw.chars() {
            let char_width = ch.width().unwrap_or(0);
            if used + char_width > width {
                lines.push(line);
                line = String::new();
                used = 0;
            }
            line.push(ch);
            used += char_width;
        }
        lines.push(line);
    }
    lines
}

fn install_command() -> &'static str {
    if cfg!(windows) {
        "powershell -c \"irm https://pi.dev/install.ps1 | iex\""
    } else {
        "curl -fsSL https://pi.dev/install.sh | sh"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DiffLine, DiffView, PointerShape, SessionHistory, SessionInfo};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn screen(app: &mut App) -> String {
        let backend = TestBackend::new(120, 35);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn working_label_animates() {
        assert_eq!(working_label(0), "working");
        assert_eq!(working_label(280), "working.");
        assert_eq!(working_label(560), "working..");
        assert_eq!(working_label(840), "working...");
        assert_eq!(working_label(1120), "working");
    }

    #[test]
    fn renders_missing_pi_state() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        let text = screen(&mut app);
        assert!(text.contains("pi not installed"));
        assert!(text.contains("https://pi.dev"));
        assert!(text.contains("Cargo.toml"));
    }

    #[test]
    fn multiline_ghost_previews_below_cursor() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root.clone(), true);
        app.open_path(&root.join("Cargo.toml"));
        app.focus = crate::app::Focus::Editor;
        if let Some(buffer) = app.editor.buffers.get_mut(app.editor.active) {
            buffer.cursor_line = 0;
            buffer.cursor_col = buffer.lines[0].chars().count();
        }
        app.editor.ghost = Some("\nsecond preview line\nthird preview line".to_string());

        let text = screen(&mut app);
        assert!(text.contains("second preview line"));
        assert!(text.contains("third preview line"));
    }

    #[test]
    fn renders_git_sidebar_and_diff() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        let text = screen(&mut app);
        assert!(text.contains("git"));
        assert!(text.contains("commit message"));

        app.diff = Some(DiffView {
            title: "M  README.md".to_string(),
            lines: vec![
                DiffLine {
                    kind: DiffKind::Add,
                    text: "+hello".to_string(),
                },
                DiffLine {
                    kind: DiffKind::Del,
                    text: "-bye".to_string(),
                },
            ],
            scroll: 0,
        });
        let text = screen(&mut app);
        assert!(text.contains("diff"));
        assert!(text.contains("+hello"));
        assert!(text.contains("-bye"));
        assert!(text.contains("esc close"));
    }

    fn click(app: &mut App, x: u16, y: u16) {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn mouse_opens_files_and_folders() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        screen(&mut app);

        let readme = app
            .tree
            .visible
            .iter()
            .position(|index| app.tree.nodes[*index].name == "README.md")
            .expect("readme row");
        let x = app.ui.tree.x + 2;
        let y = app.ui.tree_body.y + (readme - app.tree.scroll) as u16;
        click(&mut app, x, y);
        assert!(app
            .editor
            .buffer()
            .map(|buffer| buffer.path.ends_with("README.md"))
            .unwrap_or(false));

        let src = app
            .tree
            .visible
            .iter()
            .position(|index| app.tree.nodes[*index].name == "src")
            .expect("src row");
        let before = app.tree.nodes[app.tree.visible[src]].expanded;
        let y = app.ui.tree_body.y + (src - app.tree.scroll) as u16;
        click(&mut app, x, y);
        assert_ne!(app.tree.nodes[app.tree.visible[src]].expanded, before);
    }

    #[test]
    fn panels_hide_and_resize() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        screen(&mut app);
        let editor_width = app.ui.editor.width;

        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        screen(&mut app);
        assert!(app.agent_hidden);
        assert_eq!(app.ui.agent.width, 0);
        assert!(app.ui.editor.width > editor_width);

        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        screen(&mut app);
        assert!(!app.agent_hidden);

        let initial = app.left_width.unwrap();
        let tree_width = app.ui.tree.width;
        let divider = app.ui.tree_divider.unwrap();
        let y = app.ui.main.y + 2;
        click(&mut app, divider, y);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: divider + 6,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.left_width.unwrap(), initial + 6);
        screen(&mut app);
        assert!(app.ui.tree.width > tree_width);
    }

    fn move_mouse(app: &mut App, x: u16, y: u16) {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn unsupported_files_show_default_app_button() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("unsupported-{}.png", std::process::id()));
        fs::write(&path, [0u8, 1, 2, 3]).expect("write test file");
        app.open_path(&path);
        let text = screen(&mut app);
        assert!(text.contains("this file type is not supported."));
        assert!(text.contains("open in default app"));
        assert!(app.ui.unsupported_button.width > 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edits_render_before_background_highlight() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root.clone(), true);
        let path = root
            .join("target")
            .join(format!("typing-{}.txt", std::process::id()));
        fs::write(&path, "hello\n").expect("write test file");
        app.open_path(&path);
        app.editor.doc_start(false);
        app.editor.insert_char('Q');

        assert!(app.editor.buffer().unwrap().highlight_dirty);
        let text = screen(&mut app);
        assert!(text.contains("Qhello"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn edited_line_stays_plain_while_other_lines_keep_highlighting() {
        use ratatui::style::Color;
        use std::collections::HashSet;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root.clone(), true);
        let path = root
            .join("target")
            .join(format!("highlight-{}.rs", std::process::id()));
        fs::write(&path, "let first = 1;\nlet second = 2;\n").expect("write test file");
        app.open_path(&path);
        app.editor.insert_char('Q');

        let text = screen(&mut app);
        assert!(text.contains("Qlet first = 1;"));

        let backend = TestBackend::new(120, 35);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let x0 = app.ui.editor_text.x + app.ui.gutter;
        let x1 = app.ui.editor_text.x + app.ui.editor_text.width;
        let colors = |y| {
            (x0..x1)
                .filter(|x| buffer[(*x, y)].symbol() != " ")
                .map(|x| buffer[(x, y)].fg)
                .collect::<HashSet<Color>>()
        };
        assert_eq!(colors(app.ui.editor_text.y).len(), 1);
        assert!(colors(app.ui.editor_text.y + 1).len() > 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn tabs_switch_files_on_click() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        app.open_path(&root.join("README.md"));
        app.open_path(&root.join("Cargo.toml"));
        screen(&mut app);
        assert_eq!(app.ui.tabs.len(), 2);
        let (rect, index) = app.ui.tabs[1];
        click(&mut app, rect.x + 1, rect.y);
        assert_eq!(app.editor.active, index);
    }

    #[test]
    fn word_wrap_is_on_by_default_and_toggles_from_menu() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root.clone(), true);
        let path = root
            .join("target")
            .join(format!("wrap-{}.txt", std::process::id()));
        fs::write(&path, format!("WRAP_START {} WRAP_END\n", "x".repeat(180)))
            .expect("write wrap fixture");
        app.open_path(&path);
        assert!(app.editor.wrap);
        assert!(screen(&mut app).contains("WRAP_END"));

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        let text = screen(&mut app);
        assert!(text.contains("commands"));
        assert!(text.contains("Settings"));
        assert!(text.contains("LLM commands"));

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let text = screen(&mut app);
        assert!(text.contains("commands / settings"));
        assert!(text.contains("Word wrap"));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.editor.wrap);
        let text = screen(&mut app);
        assert!(text.contains("commands / settings"));
        assert!(text.contains("Word wrap"));
        assert!(!text.contains("WRAP_END"));
        let page = app
            .menu
            .as_ref()
            .and_then(|menu| menu.pages.last())
            .unwrap();
        assert_eq!(page.items[0].hint, "off");

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(screen(&mut app).contains("LLM commands"));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.menu.is_none());

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let text = screen(&mut app);
        assert!(text.contains("commands / llm commands"));
        assert!(text.contains("Explain file"));
        assert!(text.contains("Review file"));
        assert!(text.contains("Explain selection"));
        assert!(text.contains("Refactor selection"));
        assert!(text.contains("Add tests"));
        assert!(text.contains("Format file"));
        assert!(!text.contains("(Pi)"));
        let _ = fs::remove_file(path);
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn make_git_repo(name: &str) -> (PathBuf, PathBuf) {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("{name}-{}", std::process::id()));
        let repo = base.join("repo");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&repo).expect("create repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "smol@example.com"]);
        git(&repo, &["config", "user.name", "smol test"]);
        fs::write(repo.join("note.txt"), "one\n").expect("write note");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "initial"]);
        fs::write(repo.join("note.txt"), "one\ntwo\n").expect("update note");
        (base, repo)
    }

    #[test]
    fn clicking_git_change_opens_diff_in_editor() {
        let (base, repo) = make_git_repo("click-diff");
        let mut app = App::new(repo, true);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(screen(&mut app).contains("note.txt"));

        let x = app.ui.tree.x + 2;
        let y = app.ui.tree_body.y;
        click(&mut app, x, y);
        assert!(app
            .diff
            .as_ref()
            .is_some_and(|diff| diff.title.contains("note.txt")));
        let text = screen(&mut app);
        assert!(text.contains("diff"));
        assert!(text.contains("+two"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn clicking_diff_title_closes_it() {
        let (base, repo) = make_git_repo("click-diff-close");
        let mut app = App::new(repo, true);
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        screen(&mut app);
        let x = app.ui.tree.x + 2;
        let y = app.ui.tree_body.y;
        click(&mut app, x, y);
        assert!(app.diff.is_some());
        let text = screen(&mut app);
        assert!(text.contains("esc close"), "the title should offer a way out");

        let header = app.ui.diff_header;
        assert!(header.width > 0, "the diff title should be clickable");
        let title_line = text.lines().nth(header.y as usize).unwrap_or_default();
        assert!(
            title_line.contains("esc close"),
            "the diff title should show how to close it: {title_line}"
        );

        // clicking the diff body does not close it
        click(&mut app, header.x + 4, header.y + 3);
        assert!(app.diff.is_some(), "only the title closes the diff");

        // clicking the title does
        click(&mut app, header.x + 4, header.y);
        assert!(app.diff.is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn status_hints_change_with_sidebar() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        screen(&mut app);
        assert!(screen(&mut app).contains("enter open"));
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        let text = screen(&mut app);
        assert!(text.contains("enter diff"));
        assert!(text.contains("ctrl+g files"));
    }

    #[test]
    fn pointer_tracks_hover_targets() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        app.open_path(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"));
        screen(&mut app);

        let divider = app.ui.tree_divider.unwrap();
        let divider_y = app.ui.main.y + 2;
        move_mouse(&mut app, divider, divider_y);
        assert_eq!(app.pointer(), PointerShape::Resize);
        assert_eq!(app.hover_edge, Some(ResizeEdge::Sidebar));

        let tree_x = app.ui.tree_body.x + 2;
        let tree_y = app.ui.tree_body.y;
        move_mouse(&mut app, tree_x, tree_y);
        assert_eq!(app.pointer(), PointerShape::Pointer);

        let editor_x = app.ui.editor_text.x + 2;
        let editor_y = app.ui.editor_text.y + 1;
        move_mouse(&mut app, editor_x, editor_y);
        assert_eq!(app.pointer(), PointerShape::Text);

        let status_y = app.ui.status.y;
        move_mouse(&mut app, 2, status_y);
        assert_eq!(app.pointer(), PointerShape::Default);
    }

    #[test]
    fn agent_renders_tagged_prompt_blocks() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        app.agent.entries.push(Entry::User(
            "Review this code.\n\n<selection>\nfn main() {}\n</selection>\n\n<context>\nlet x = 1;\n</context>"
                .to_string(),
        ));
        let text = screen(&mut app);
        assert!(text.contains("Review this code."));
        assert!(text.contains("selection"));
        assert!(text.contains("fn main() {}"));
        assert!(text.contains("context"));
        assert!(text.contains("let x = 1;"));
        assert!(!text.contains("<selection>"));
        assert!(!text.contains("</selection>"));
        assert!(!text.contains("<context>"));
    }

    #[test]
    fn thinking_header_toggles_on_click() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        app.agent.entries.push(Entry::Assistant {
            text: "the answer".to_string(),
            thinking: "REASONING_MARKER".to_string(),
            thinking_open: true,
        });
        assert!(screen(&mut app).contains("REASONING_MARKER"));

        let (x, y) = app
            .ui
            .agent_thinking
            .first()
            .map(|(rect, _)| (rect.x + 2, rect.y))
            .expect("thinking header");
        move_mouse(&mut app, x, y);
        assert_eq!(app.pointer(), PointerShape::Pointer);
        click(&mut app, x, y);
        let text = screen(&mut app);
        assert!(!text.contains("REASONING_MARKER"));
        assert!(text.contains("the answer"));
        assert!(matches!(
            app.agent.entries.last(),
            Some(Entry::Assistant {
                thinking_open: false,
                ..
            })
        ));

        let (x, y) = app
            .ui
            .agent_thinking
            .first()
            .map(|(rect, _)| (rect.x + 2, rect.y))
            .expect("thinking header");
        click(&mut app, x, y);
        assert!(screen(&mut app).contains("REASONING_MARKER"));
        assert!(matches!(
            app.agent.entries.last(),
            Some(Entry::Assistant {
                thinking_open: true,
                ..
            })
        ));
    }

    #[test]
    fn agent_renders_markdown() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        app.agent.entries.push(Entry::Assistant {
            text: "# Heading\n\n- first item\n\n| name | qty |\n|---|---|\n| pear | 2 |\n\nUse ``smol --help`` here.\n\n```rust\nfn main() {}\n```"
                .to_string(),
            thinking: "THINKING_MARKER".to_string(),
            thinking_open: true,
        });
        let text = screen(&mut app);
        assert!(text.contains("thinking"));
        assert!(text.contains("THINKING_MARKER"));
        assert!(text.contains("Heading"));
        assert!(text.contains("first item"));
        assert!(text.contains("pear"));
        assert!(text.contains("smol --help"));
        assert!(!text.contains("``"));
        assert!(text.contains("fn main()"));
    }

    #[test]
    fn file_suggestion_popup_renders_paths() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        app.ui.agent = Rect::new(0, 5, 50, 12);
        app.agent.suggestions = vec!["src/app.rs".to_string(), "src/tree.rs".to_string()];
        app.agent.suggestion_selected = 1;
        let input = Rect::new(0, 15, 50, 1);

        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_agent_suggestions(frame, input, &app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("files"));
        assert!(text.contains("@src/app.rs"));
        assert!(text.contains("@src/tree.rs"));
    }

    #[test]
    fn explorer_right_click_renders_menu_and_rename_dialog() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        let readme = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
        app.tree.reveal(&readme);
        let position = app
            .tree
            .visible
            .iter()
            .position(|index| app.tree.nodes[*index].path == readme)
            .expect("README row");
        screen(&mut app);

        let y = app.ui.tree_body.y + (position - app.tree.scroll) as u16;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: app.ui.tree_body.x + 2,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
        let text = screen(&mut app);
        assert!(text.contains("Open in default app"));
        assert!(text.contains("Copy relative path"));

        for _ in 0..4 {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let text = screen(&mut app);
        assert!(text.contains("rename README.md"));
        assert!(text.contains("enter confirm"));
    }

    /// Dumps the overlays as text for eyeballing layout; run with
    /// `cargo test dump_screens -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_screens() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        let mut app = App::new(root.clone(), true);
        app.models = vec![
            serde_json::json!({ "provider": "home-server", "id": "qwen35-4b", "name": "Qwen 3.5 4B Local", "contextWindow": 262000 }),
            serde_json::json!({ "provider": "vercel-ai-gateway", "id": "zai/glm-5.3", "name": "GLM 5.3" }),
            serde_json::json!({ "provider": "lfm", "id": "lfm3", "name": "LFM3 1.7B" }),
        ];
        app.model_current = Some(crate::app::ModelPref {
            provider: "home-server".to_string(),
            id: "qwen35-4b".to_string(),
            name: "Qwen 3.5 4B Local".to_string(),
            context_window: Some(262_000),
        });
        app.open_model_picker();
        println!("--- model picker\n{}", screen(&mut app));

        app.picker = None;
        app.thinking_level = Some("medium".to_string());
        app.open_thinking_picker();
        println!("--- thinking picker\n{}", screen(&mut app));

        app.picker = None;
        app.open_provider_form(crate::app::ProviderPrefill {
            id: "home-server".to_string(),
            name: "Home server".to_string(),
            base_url: "http://100.67.79.86:8080/v1".to_string(),
            api_key: "local".to_string(),
            api: "openai-completions".to_string(),
            model_id: "qwen35-4b".to_string(),
            model_name: "Qwen 3.5 4B Local".to_string(),
            context: "262000".to_string(),
        });
        println!("--- provider dialog\n{}", screen(&mut app));

        let mut app = App::new(root, true);
        app.agent.entries.push(crate::app::Entry::User(
            "add a todo panel please".to_string(),
        ));
        let start = std::time::Instant::now();
        let tasks = |statuses: [crate::todo::TodoStatus; 4]| {
            [
                "explore the repo",
                "write the parser with tests",
                "render the panel",
                "update the docs",
            ]
            .iter()
            .zip(statuses)
            .map(|(title, status)| crate::todo::TodoTask {
                title: (*title).to_string(),
                status,
            })
            .collect::<Vec<_>>()
        };
        use crate::todo::TodoStatus::{Active, Done, Pending};
        app.todo.apply_read(tasks([Pending, Active, Pending, Pending]), start);
        app.todo.apply_read(
            tasks([Done, Active, Pending, Pending]),
            start + std::time::Duration::from_secs(42),
        );
        app.now = start + std::time::Duration::from_secs(300);
        println!("--- todo panel\n{}", screen(&mut app));
    }

    #[test]
    fn provider_dialog_renders_the_form() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root, true);
        app.open_provider_form(crate::app::ProviderPrefill {
            id: "home".to_string(),
            name: "Home server".to_string(),
            base_url: "http://100.67.79.86:8080/v1".to_string(),
            api_key: "local".to_string(),
            api: "openai-completions".to_string(),
            model_id: "qwen35-4b".to_string(),
            model_name: "Qwen 3.5 4B Local".to_string(),
            context: "262000".to_string(),
        });

        let text = screen(&mut app);
        assert!(text.contains("connect provider"), "{text}");
        for label in crate::app::PROVIDER_LABELS {
            assert!(text.contains(label), "missing {label}: {text}");
        }
        assert!(text.contains("home"), "{text}");
        assert!(text.contains("http://100.67.79.86:8080/v1"), "{text}");
        assert!(text.contains("262000"), "{text}");
        assert!(text.contains("enter save"), "{text}");
    }

    #[test]
    fn picker_overlay_renders_models() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root, true);
        app.models = vec![
            serde_json::json!({ "provider": "zai", "id": "glm-4.6", "name": "GLM 4.6" }),
            serde_json::json!({
                "provider": "anthropic",
                "id": "claude-sonnet-4",
                "name": "Claude Sonnet 4",
                "contextWindow": 200000
            }),
        ];
        app.model_current = Some(crate::app::ModelPref {
            provider: "anthropic".to_string(),
            id: "claude-sonnet-4".to_string(),
            name: "Claude Sonnet 4".to_string(),
            context_window: Some(200_000),
        });
        app.open_model_picker();

        let text = screen(&mut app);
        assert!(text.contains("model · 2"));
        assert!(text.contains("GLM 4.6"));
        assert!(text.contains("Claude Sonnet 4"));
        assert!(text.contains("200k"));
        assert!(text.contains("enter switch"));
        // The active model is starred and the list is clickable.
        assert!(text.contains("* Claude Sonnet 4") || text.contains(" *Claude"));
        assert_eq!(app.ui.picker_rows.len(), 2);
        assert!(app.ui.picker.width > 0);
    }

    #[test]
    fn todo_panel_shows_progress_and_durations() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root, true);
        let start = std::time::Instant::now();
        let pending = |title: &str| crate::todo::TodoTask {
            title: title.to_string(),
            status: crate::todo::TodoStatus::Pending,
        };
        let mut active = pending("write the parser");
        active.status = crate::todo::TodoStatus::Active;

        app.todo.apply_read(
            vec![pending("explore the repo"), active.clone(), pending("ship it")],
            start,
        );
        app.todo.apply_read(
            vec![
                crate::todo::TodoTask {
                    title: "explore the repo".to_string(),
                    status: crate::todo::TodoStatus::Done,
                },
                active,
                pending("ship it"),
            ],
            start + std::time::Duration::from_secs(30),
        );
        app.now = start + std::time::Duration::from_secs(75);

        let text = screen(&mut app);
        assert!(text.contains("todos 1/3"), "heading missing: {text}");
        assert!(text.contains("33%"), "progress missing: {text}");
        assert!(text.contains("[x] explore the repo"), "{text}");
        assert!(text.contains("30s"), "finished duration missing: {text}");
        assert!(text.contains("[>] write the parser"), "{text}");
        assert!(text.contains("1m15s"), "live duration missing: {text}");
        assert!(text.contains("[ ] ship it"), "{text}");
    }

    #[test]
    fn todo_panel_stays_hidden_without_tasks() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root, true);
        let text = screen(&mut app);
        assert!(!text.contains("todos "), "no panel expected: {text}");
    }

    #[test]
    fn session_history_overlay_renders_sessions() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root.clone(), true);
        let active = root.join("active.jsonl");
        let second = root.join("second.jsonl");
        app.session_path = Some(active.clone());
        app.history = Some(SessionHistory {
            sessions: vec![
                SessionInfo {
                    path: active,
                    cwd: root.to_string_lossy().to_string(),
                    name: Some("fix parser".to_string()),
                    first_message: "first prompt".to_string(),
                    message_count: 4,
                    modified: std::time::SystemTime::now(),
                },
                SessionInfo {
                    path: second,
                    cwd: root.to_string_lossy().to_string(),
                    name: None,
                    first_message: "second prompt".to_string(),
                    message_count: 1,
                    modified: std::time::SystemTime::now(),
                },
            ],
            selected: 1,
            confirm: None,
        });

        let text = screen(&mut app);
        assert!(text.contains("session history"));
        assert!(text.contains("fix parser"));
        assert!(text.contains("second prompt"));
        assert!(text.contains("enter resume"));
        assert_eq!(app.ui.history_rows.len(), 2);
        assert!(app.ui.history.width > 0);
    }
}
