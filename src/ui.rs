use std::path::Path;

use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, DiffKind, Entry, Focus, PiState, ResizeEdge, Sidebar, ToolState, Track, TrackKind,
};
use crate::editor::{display_width, Buffer};
use crate::theme;

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

    draw_sidebar(frame, tree_rect, app);
    draw_editor(frame, editor_rect, app);
    if !app.agent_hidden {
        draw_agent(frame, agent_rect, app);
    }
    draw_menu(frame, main, app);
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
            let hovered = app.hover.map_or(false, |(hx, hy)| {
                hx >= rect.x && hx < rect.x + rect.width && hy == rect.y
            });
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
    app.editor
        .ensure_visible(text_area.height as usize, text_width);
    let rows = app.editor.visual_rows(text_width);
    let wrap = app.editor.wrap;

    let tracks = &app.tracks;
    let Some(buffer) = app.editor.buffer() else {
        return;
    };
    let cursor = app.editor.cursor_visual(text_width);
    let ghost = app.editor.ghost.as_deref();
    let height = text_area.height as usize;

    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let display_row = buffer.scroll + row;
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

        if focused
            && cursor.map(|(cursor_row, _)| cursor_row == display_row) == Some(true)
            && buffer.selection.is_none()
        {
            if let (Some(ghost), Some((_, column))) = (ghost, cursor) {
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
                    let visible = clip_display(ghost, text_width.saturating_sub(start));
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
    let hovered = app.hover.map_or(false, |(x, y)| {
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
        PiState::Running(_) if app.agent.busy => ("working", theme::ACCENT),
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
    let body = Rect {
        x: content.x,
        y: content.y + 1,
        width: content.width,
        height: content.height.saturating_sub(1 + input_height),
    };

    app.ui.agent_thinking.clear();
    match &app.pi {
        PiState::Missing if app.agent.entries.is_empty() => draw_missing(frame, body),
        PiState::Failed(error) if app.agent.entries.is_empty() => draw_failed(frame, body, error),
        _ => draw_conversation(frame, body, app),
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
                    lines.push(Line::from(Span::styled(
                        "pi",
                        Style::default()
                            .fg(theme::GREEN)
                            .add_modifier(Modifier::BOLD),
                    )));
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
                for part in wrap_text(&tool.label, width) {
                    lines.push(Line::from(Span::styled(part, Style::default().fg(color))));
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
    let hovered = app.hover.map_or(false, |(x, y)| {
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
                clip_display(&diff.title, area.width.saturating_sub(6) as usize),
                Style::default().fg(theme::DIM),
            ),
        ]))
        .style(Style::default().bg(theme::BG)),
        header,
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
    let width = area.width.saturating_sub(8).min(64).max(32).min(area.width);
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
    use crate::app::{DiffLine, DiffView, PointerShape};
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
    fn renders_missing_pi_state() {
        let mut app = App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true);
        let text = screen(&mut app);
        assert!(text.contains("pi not installed"));
        assert!(text.contains("https://pi.dev"));
        assert!(text.contains("Cargo.toml"));
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
}
