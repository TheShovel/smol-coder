use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use serde_json::{json, Value};

use crate::editor::Editor;
use crate::git::Git;
use crate::pi::{find_pi, PiClient, PiEvent};
use crate::tree::FileTree;

const AUTO_COMPACT_PERCENT: u64 = 95;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Tree,
    Editor,
    Agent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sidebar {
    Files,
    Git,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResizeEdge {
    Sidebar,
    Agent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointerShape {
    Default,
    Pointer,
    Text,
    Resize,
}

impl PointerShape {
    pub fn css(self) -> &'static str {
        match self {
            PointerShape::Default => "default",
            PointerShape::Pointer => "pointer",
            PointerShape::Text => "text",
            PointerShape::Resize => "ew-resize",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackKind {
    Read,
    Write,
}

pub struct Track {
    pub path: PathBuf,
    pub start: usize,
    pub end: usize,
    pub kind: TrackKind,
    pub active: bool,
    pub until: Instant,
}

pub enum PiState {
    Missing,
    Failed(String),
    Running(PiClient),
}

#[derive(Default, Clone)]
pub struct UiRects {
    pub main: Rect,
    pub tree: Rect,
    pub tree_body: Rect,
    pub tree_divider: Option<u16>,
    pub agent_divider: Option<u16>,
    pub git_input: Rect,
    pub editor: Rect,
    pub editor_text: Rect,
    pub tabs: Vec<(Rect, usize)>,
    pub unsupported_button: Rect,
    pub gutter: u16,
    pub agent: Rect,
    pub agent_input: Rect,
    pub agent_thinking: Vec<(Rect, usize)>,
    pub status: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

pub struct ToolEntry {
    pub label: String,
    pub state: ToolState,
}

pub enum Entry {
    User(String),
    Assistant {
        text: String,
        thinking: String,
        thinking_open: bool,
    },
    Tool(ToolEntry),
    Note(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Meta,
    Add,
    Del,
    Hunk,
    Context,
}

pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

pub struct DiffView {
    pub title: String,
    pub lines: Vec<DiffLine>,
    pub scroll: usize,
}

#[derive(Clone, Copy)]
pub enum MenuAction {
    OpenMenu(MenuPageKind),
    ToggleWrap,
    ToggleAgent,
    ToggleGit,
    Save,
    CloseFile,
    NextFile,
    PreviousFile,
    RefreshGit,
    ExplainFile,
    ReviewFile,
    ExplainSelection,
    RefactorSelection,
    AddTests,
    FormatFile,
    Quit,
}

#[derive(Clone, Copy)]
pub enum MenuPageKind {
    Files,
    Settings,
    Llm,
    Git,
}

pub struct MenuItem {
    pub label: String,
    pub hint: String,
    pub action: MenuAction,
}

pub struct MenuPage {
    pub kind: Option<MenuPageKind>,
    pub title: &'static str,
    pub selected: usize,
    pub items: Vec<MenuItem>,
}
pub struct CommandMenu {
    pub pages: Vec<MenuPage>,
}

fn submenu_item(label: &str, count: usize, kind: MenuPageKind) -> MenuItem {
    MenuItem {
        label: label.to_string(),
        hint: format!("{count} items"),
        action: MenuAction::OpenMenu(kind),
    }
}

pub struct PiCommand {
    pub name: String,
    pub description: String,
}

pub struct Agent {
    pub entries: Vec<Entry>,
    pub input: String,
    pub input_cursor: usize,
    pub scroll: usize,
    pub busy: bool,
    streaming: Option<usize>,
    tools: HashMap<String, usize>,
}

impl Agent {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            scroll: 0,
            busy: false,
            streaming: None,
            tools: HashMap::new(),
        }
    }

    fn push_user(&mut self, text: &str) {
        self.entries.push(Entry::User(text.to_string()));
        self.scroll = 0;
    }

    fn note(&mut self, text: impl Into<String>) {
        self.entries.push(Entry::Note(text.into()));
        self.scroll = 0;
    }

    fn assistant_start(&mut self) {
        let reuse = self.entries.last().is_some_and(
            |entry| matches!(entry, Entry::Assistant { text, .. } if text.trim().is_empty()),
        );
        if reuse {
            self.streaming = Some(self.entries.len() - 1);
            self.scroll = 0;
            return;
        }
        self.entries.push(Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            thinking_open: false,
        });
        self.streaming = Some(self.entries.len() - 1);
        self.scroll = 0;
    }

    fn assistant_delta(&mut self, delta: &str) {
        if self.streaming.is_none() {
            self.assistant_start();
        }
        if let Some(index) = self.streaming {
            if let Some(Entry::Assistant { text, .. }) = self.entries.get_mut(index) {
                text.push_str(delta);
            }
        }
        self.scroll = 0;
    }

    fn thinking_start(&mut self) {
        if self.streaming.is_none() {
            self.assistant_start();
        }
        if let Some(index) = self.streaming {
            if let Some(Entry::Assistant { thinking_open, .. }) = self.entries.get_mut(index) {
                *thinking_open = true;
            }
        }
        self.scroll = 0;
    }

    fn thinking_delta(&mut self, delta: &str) {
        self.thinking_start();
        if let Some(index) = self.streaming {
            if let Some(Entry::Assistant { thinking, .. }) = self.entries.get_mut(index) {
                thinking.push_str(delta);
            }
        }
    }

    fn thinking_final(&mut self, text: String) {
        if self.streaming.is_none() && text.is_empty() {
            return;
        }
        self.thinking_start();
        if let Some(index) = self.streaming {
            if let Some(Entry::Assistant {
                thinking,
                thinking_open,
                ..
            }) = self.entries.get_mut(index)
            {
                if !text.is_empty() {
                    *thinking = text;
                }
                *thinking_open = false;
            }
        }
    }

    fn assistant_final(&mut self, text: String) {
        if let Some(index) = self.streaming.take() {
            if let Some(Entry::Assistant {
                text: existing,
                thinking_open,
                ..
            }) = self.entries.get_mut(index)
            {
                *existing = text;
                *thinking_open = false;
            }
        } else if !text.is_empty() {
            self.entries.push(Entry::Assistant {
                text,
                thinking: String::new(),
                thinking_open: false,
            });
        }
    }

    fn toggle_thinking(&mut self, index: usize) -> bool {
        let Some(Entry::Assistant {
            thinking,
            thinking_open,
            ..
        }) = self.entries.get_mut(index)
        else {
            return false;
        };
        if thinking.trim().is_empty() {
            return false;
        }
        *thinking_open = !*thinking_open;
        true
    }

    fn push_tool(&mut self, id: &str, label: String, state: ToolState) {
        let index = self.entries.len();
        self.entries.push(Entry::Tool(ToolEntry { label, state }));
        self.tools.insert(id.to_string(), index);
        self.scroll = 0;
    }

    fn update_tool(&mut self, id: &str, state: ToolState) {
        if let Some(index) = self.tools.get(id).copied() {
            if let Some(Entry::Tool(tool)) = self.entries.get_mut(index) {
                tool.state = state;
            }
        }
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }

    fn insert_input(&mut self, ch: char) {
        let mut chars: Vec<char> = self.input.chars().collect();
        let col = self.input_cursor.min(chars.len());
        chars.insert(col, ch);
        self.input = chars.into_iter().collect();
        self.input_cursor = col + 1;
    }

    fn backspace_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        let col = self.input_cursor.min(chars.len());
        chars.remove(col - 1);
        self.input = chars.into_iter().collect();
        self.input_cursor = col - 1;
    }

    fn delete_input(&mut self) {
        let mut chars: Vec<char> = self.input.chars().collect();
        if self.input_cursor < chars.len() {
            chars.remove(self.input_cursor);
            self.input = chars.into_iter().collect();
        }
    }

    fn move_input(&mut self, delta: isize) {
        let length = self.input.chars().count();
        let next = self.input_cursor as isize + delta;
        self.input_cursor = next.clamp(0, length as isize) as usize;
    }

    fn input_home(&mut self) {
        self.input_cursor = 0;
    }

    fn input_end(&mut self) {
        self.input_cursor = self.input.chars().count();
    }

    fn scroll_by(&mut self, delta: isize) {
        let next = self.scroll as isize + delta;
        self.scroll = next.max(0) as usize;
    }

    fn reset(&mut self) {
        self.entries.clear();
        self.clear_input();
        self.scroll = 0;
        self.busy = false;
        self.streaming = None;
        self.tools.clear();
    }

    fn last_assistant(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Assistant { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
    }
}

struct PendingTool {
    kind: Option<TrackKind>,
    path: Option<PathBuf>,
    before: Option<String>,
    start: usize,
    end: usize,
}

pub struct App {
    pub root: PathBuf,
    pub tree: FileTree,
    pub editor: Editor,
    pub agent: Agent,
    pub git: Git,
    pub pi: PiState,
    pub focus: Focus,
    pub sidebar: Sidebar,
    pub should_quit: bool,
    pub status: Option<(String, Instant)>,
    pub tracks: Vec<Track>,
    pub ui: UiRects,
    pub now: Instant,
    pub model: Option<String>,
    pub diff: Option<DiffView>,
    pub menu: Option<CommandMenu>,
    pub unsupported: Option<PathBuf>,
    pub dragging: bool,
    pub agent_hidden: bool,
    pub left_width: Option<u16>,
    pub right_width: Option<u16>,
    pub resizing: Option<ResizeEdge>,
    pub hover: Option<(u16, u16)>,
    pub hover_edge: Option<ResizeEdge>,
    pointer: PointerShape,
    pointer_dirty: bool,
    pub commands: Vec<PiCommand>,
    pub models: Vec<Value>,
    context_percent: Option<u64>,
    stats_notice: bool,
    auto_compacted: bool,
    pending: HashMap<String, PendingTool>,
    pi_stderr: Vec<String>,
    pi_exited: bool,
}

impl App {
    pub fn new(root: PathBuf, force_no_pi: bool) -> Self {
        let tree = FileTree::new(&root);
        let editor = Editor::new(&root);
        let git = Git::new(&root);
        let pi = if force_no_pi {
            PiState::Missing
        } else {
            match find_pi() {
                Some(path) => match PiClient::spawn(&path, &root) {
                    Ok(mut client) => {
                        let _ = client.get_state();
                        let _ = client.send(json!({ "type": "get_commands" }));
                        let _ =
                            client.send(json!({ "type": "set_auto_compaction", "enabled": false }));
                        let _ = client.send(json!({ "type": "get_session_stats" }));
                        PiState::Running(client)
                    }
                    Err(error) => PiState::Failed(format!("could not start pi: {error}")),
                },
                None => PiState::Missing,
            }
        };

        Self {
            root,
            tree,
            editor,
            agent: Agent::new(),
            git,
            pi,
            focus: Focus::Tree,
            sidebar: Sidebar::Files,
            should_quit: false,
            status: None,
            tracks: Vec::new(),
            ui: UiRects::default(),
            now: Instant::now(),
            model: None,
            diff: None,
            menu: None,
            unsupported: None,
            dragging: false,
            agent_hidden: false,
            left_width: None,
            right_width: None,
            resizing: None,
            hover: None,
            hover_edge: None,
            pointer: PointerShape::Default,
            pointer_dirty: false,
            commands: Vec::new(),
            models: Vec::new(),
            context_percent: None,
            stats_notice: false,
            auto_compacted: false,
            pending: HashMap::new(),
            pi_stderr: Vec::new(),
            pi_exited: false,
        }
    }

    pub fn shutdown(&mut self) {
        if let PiState::Running(client) = &mut self.pi {
            client.shutdown();
        }
    }

    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Instant::now()));
    }

    pub fn drain_pi(&mut self) {
        let mut events = Vec::new();
        if let PiState::Running(client) = &self.pi {
            while let Ok(event) = client.rx.try_recv() {
                events.push(event);
            }
        }

        for event in events {
            match event {
                PiEvent::Record(value) => self.on_pi_record(value),
                PiEvent::Stderr(line) => {
                    self.pi_stderr.push(line);
                    if self.pi_stderr.len() > 20 {
                        self.pi_stderr.remove(0);
                    }
                }
                PiEvent::Exit => self.pi_exited = true,
            }
        }

        if self.pi_exited && matches!(self.pi, PiState::Running(_)) {
            let detail = self
                .pi_stderr
                .last()
                .cloned()
                .unwrap_or_else(|| "pi exited".to_string());
            self.pi = PiState::Failed(detail.clone());
            self.agent.busy = false;
            self.agent.note(detail);
        }
    }

    fn on_pi_record(&mut self, value: Value) {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "response" => self.on_response(&value),
            "message_start" => {
                let role = value
                    .pointer("/message/role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if role == "assistant" {
                    self.agent.assistant_start();
                    self.agent.busy = true;
                }
            }
            "message_update" => {
                let event = value.get("assistantMessageEvent");
                let event_type = event
                    .and_then(|event| event.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match event_type {
                    "text_delta" => {
                        if let Some(delta) = event
                            .and_then(|event| event.get("delta"))
                            .and_then(Value::as_str)
                        {
                            self.agent.assistant_delta(delta);
                        }
                    }
                    "thinking_start" => self.agent.thinking_start(),
                    "thinking_delta" => {
                        if let Some(delta) = event
                            .and_then(|event| event.get("delta"))
                            .and_then(Value::as_str)
                        {
                            self.agent.thinking_delta(delta);
                        }
                    }
                    "thinking_end" => {
                        if let Some(content) = event
                            .and_then(|event| event.get("content"))
                            .and_then(Value::as_str)
                        {
                            self.agent.thinking_final(content.to_string());
                        }
                    }
                    "error" => {
                        if let Some(error) = event
                            .and_then(|event| event.get("error"))
                            .and_then(Value::as_str)
                        {
                            self.agent.note(error.to_string());
                            self.agent.busy = false;
                        }
                    }
                    _ => {}
                }
            }
            "message_end" => {
                let role = value
                    .pointer("/message/role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if role == "assistant" {
                    let text = value
                        .pointer("/message")
                        .map(message_text)
                        .unwrap_or_default();
                    self.agent.assistant_final(text);
                }
            }
            "tool_execution_start" => self.on_tool_start(&value),
            "tool_execution_end" => self.on_tool_end(&value),
            "agent_start" => self.agent.busy = true,
            "agent_settled" => {
                self.agent.busy = false;
                self.agent.scroll = 0;
                self.request_context_stats(false);
            }
            "auto_retry_start" => {
                let attempt = value.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                let max = value
                    .get("maxAttempts")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let error = value
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.agent.note(format!("retry {attempt}/{max} {error}"));
            }
            "extension_error" => {
                if let Some(error) = value.get("error").and_then(Value::as_str) {
                    self.agent.note(error.to_string());
                }
            }
            "compaction_start" => self.agent.note("compacting context"),
            "compaction_end" => {
                if let Some(error) = value.get("errorMessage").and_then(Value::as_str) {
                    self.agent.note(format!("compaction failed: {error}"));
                }
                self.request_context_stats(false);
            }
            _ => {}
        }
    }

    fn on_response(&mut self, value: &Value) {
        let command = value
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let success = value
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if !success {
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            if command != "set_auto_compaction" {
                self.agent.note(format!("{command}: {error}"));
            }
            if command == "prompt" || command == "abort" {
                self.agent.busy = false;
            }
            return;
        }

        match command {
            "get_state" => {
                if let Some(model) = value.pointer("/data/model") {
                    self.model = model
                        .get("name")
                        .or_else(|| model.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                if let Some(streaming) = value.pointer("/data/isStreaming").and_then(Value::as_bool)
                {
                    self.agent.busy = streaming;
                }
            }
            "get_commands" => {
                self.commands.clear();
                if let Some(commands) = value.pointer("/data/commands").and_then(Value::as_array) {
                    for command in commands {
                        if let Some(name) = command.get("name").and_then(Value::as_str) {
                            self.commands.push(PiCommand {
                                name: name.to_string(),
                                description: command
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            });
                        }
                    }
                }
            }
            "get_available_models" => {
                self.models = value
                    .pointer("/data/models")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let labels: Vec<String> = self
                    .models
                    .iter()
                    .take(20)
                    .filter_map(|model| {
                        let provider = model.get("provider").and_then(Value::as_str)?;
                        let id = model.get("id").and_then(Value::as_str)?;
                        Some(format!("{provider}/{id}"))
                    })
                    .collect();
                if labels.is_empty() {
                    self.agent.note("no models available");
                } else {
                    self.agent.note(format!("models\n{}", labels.join("\n")));
                }
            }
            "set_model" => {
                let name = value
                    .pointer("/data/name")
                    .and_then(Value::as_str)
                    .or_else(|| value.pointer("/data/id").and_then(Value::as_str));
                if let Some(name) = name {
                    self.model = Some(name.to_string());
                    self.set_status(format!("model {name}"));
                } else {
                    self.set_status("model changed");
                }
            }
            "compact" => {
                self.agent.note("context compacted");
                self.request_context_stats(false);
            }
            "new_session" => {
                self.agent.reset();
                self.context_percent = None;
                self.auto_compacted = false;
                self.set_status("new session");
            }
            "set_thinking_level" => {
                if let Some(level) = value.pointer("/data/level").and_then(Value::as_str) {
                    self.agent.note(format!("thinking {level}"));
                }
            }
            "get_session_stats" => {
                let percent = value
                    .pointer("/data/contextUsage/percent")
                    .and_then(Value::as_u64);
                self.context_percent = percent;
                if std::mem::take(&mut self.stats_notice) {
                    let messages = value
                        .pointer("/data/totalMessages")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let tokens = value
                        .pointer("/data/tokens/total")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    self.agent.note(format!(
                        "session  {messages} messages  {tokens} tokens  {}% context",
                        percent.unwrap_or(0)
                    ));
                }
                if let Some(percent) = percent {
                    self.maybe_auto_compact(percent);
                }
            }
            "set_session_name" => self.set_status("session named"),
            "export_html" => {
                if let Some(path) = value.pointer("/data/path").and_then(Value::as_str) {
                    self.agent.note(format!("exported {path}"));
                }
            }
            "prompt" => {
                let disposition = value
                    .pointer("/data/disposition")
                    .and_then(Value::as_str)
                    .unwrap_or("started");
                if disposition == "handled" {
                    self.agent.busy = false;
                }
            }
            "abort" => {
                self.agent.busy = false;
                self.set_status("agent stopped");
            }
            _ => {}
        }
    }

    fn on_tool_start(&mut self, value: &Value) {
        let id = value
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = value
            .get("toolName")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let args = value.get("args").cloned().unwrap_or(Value::Null);
        let path = tool_path(&args).map(|raw| self.resolve(&raw));
        let kind = match name {
            "read" => Some(TrackKind::Read),
            "write" | "edit" => Some(TrackKind::Write),
            _ => None,
        };
        let label = tool_label(name, &args, path.as_deref(), &self.root);
        self.agent.push_tool(&id, label, ToolState::Running);

        let Some(path) = path else {
            return;
        };

        let before = fs::read_to_string(&path).ok();
        let (start, end) = if kind == Some(TrackKind::Read) {
            read_range(&args, before.as_deref())
        } else {
            let total = before
                .as_deref()
                .map(|content| content.lines().count())
                .unwrap_or(0)
                .max(1);
            (1, total)
        };

        if let Some(kind) = kind {
            self.set_track_start(path.clone(), kind, start, end);
            if path.is_file() {
                self.follow_file(&path);
            }
        }

        self.pending.insert(
            id,
            PendingTool {
                kind,
                path: Some(path),
                before,
                start,
                end,
            },
        );
    }

    fn on_tool_end(&mut self, value: &Value) {
        let id = value
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let failed = value
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.agent.update_tool(
            &id,
            if failed {
                ToolState::Failed
            } else {
                ToolState::Done
            },
        );

        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        let Some(path) = pending.path else {
            return;
        };

        if let Some(kind) = pending.kind {
            let after = fs::read_to_string(&path).ok();
            let (start, end) = if kind == TrackKind::Read {
                (pending.start, pending.end)
            } else {
                changed_range(pending.before.as_deref(), after.as_deref())
                    .unwrap_or((pending.start, pending.end))
            };
            self.set_track_end(path.clone(), kind, start, end);

            if kind == TrackKind::Write {
                self.tree.refresh();
                self.reload_after_tool(&path);
            }
        }
    }

    fn follow_file(&mut self, path: &Path) {
        if path.strip_prefix(&self.root).is_err() {
            return;
        }
        if let Some(buffer) = self.editor.buffer() {
            if buffer.dirty && buffer.path != path {
                return;
            }
        }
        let _ = self.editor.open(path);
        self.tree.reveal(path);
    }

    fn reload_after_tool(&mut self, path: &Path) {
        let Some(index) = self.editor.find_buffer(path) else {
            return;
        };
        if self.editor.buffers[index].dirty {
            self.editor.buffers[index].external_change = true;
            self.set_status(format!("{} changed on disk", self.rel(path)));
        } else if let Err(error) = self.editor.reload_at(index) {
            self.set_status(error);
        }
    }

    fn resolve(&self, raw: &str) -> PathBuf {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        }
    }

    pub fn rel(&self, path: &Path) -> String {
        self.editor.rel_of(path)
    }

    fn set_track_start(&mut self, path: PathBuf, kind: TrackKind, start: usize, end: usize) {
        self.tracks
            .retain(|track| track.path != path || track.active);
        self.tracks.push(Track {
            path,
            start,
            end,
            kind,
            active: true,
            until: self.now + Duration::from_secs(120),
        });
        if self.tracks.len() > 6 {
            self.tracks.remove(0);
        }
    }

    fn set_track_end(&mut self, path: PathBuf, kind: TrackKind, start: usize, end: usize) {
        let until = self.now + Duration::from_secs(3);
        if let Some(track) = self
            .tracks
            .iter_mut()
            .rev()
            .find(|track| track.path == path && track.kind == kind)
        {
            track.start = start;
            track.end = end;
            track.active = false;
            track.until = until;
        } else {
            self.tracks.push(Track {
                path,
                start,
                end,
                kind,
                active: false,
                until,
            });
        }
    }

    pub fn track_label_for(&self, path: &Path) -> Option<String> {
        let track = self
            .tracks
            .iter()
            .rev()
            .find(|track| track.path == path && (track.active || track.until > self.now))?;
        let kind = match track.kind {
            TrackKind::Read => "read",
            TrackKind::Write => "write",
        };
        Some(format!("{kind} {}-{}", track.start, track.end))
    }

    pub fn tick(&mut self) {
        self.now = Instant::now();

        let clear_status = self
            .status
            .as_ref()
            .map(|(_, at)| self.now.duration_since(*at) > Duration::from_secs(5))
            .unwrap_or(false);
        if clear_status {
            self.status = None;
        }

        for track in &mut self.tracks {
            if track.active && self.now >= track.until {
                track.active = false;
            }
        }
        self.tracks
            .retain(|track| track.active || track.until > self.now);

        self.editor.tick(self.now);

        if let Some(result) = self.git.poll_push() {
            match result {
                Ok(message) => {
                    self.set_status(message);
                    self.git.refresh();
                }
                Err(error) => self.set_status(error),
            }
        }
        if self.git.repo
            && !self.git.pushing
            && self.git.last_refresh.elapsed() >= Duration::from_secs(2)
        {
            self.git.refresh();
        }
    }

    pub fn send_prompt(&mut self) {
        let text = self.agent.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if text.starts_with('/') {
            self.handle_agent_command(&text);
            return;
        }
        if matches!(self.pi, PiState::Missing) {
            self.set_status("pi is not installed");
            return;
        }
        if let PiState::Failed(error) = &self.pi {
            self.set_status(error.clone());
            return;
        }

        self.agent.push_user(&text);
        self.agent.clear_input();
        self.send_text_prompt(&text);
        self.agent.scroll = 0;
    }

    fn send_text_prompt(&mut self, text: &str) {
        let busy = self.agent.busy;
        if let PiState::Running(client) = &mut self.pi {
            match client.prompt(text, busy) {
                Ok(()) => self.agent.busy = true,
                Err(error) => {
                    self.agent.note(format!("prompt failed: {error}"));
                    self.agent.busy = false;
                }
            }
        }
        self.agent.scroll = 0;
    }

    fn request_context_stats(&mut self, announce: bool) {
        if !matches!(self.pi, PiState::Running(_)) {
            return;
        }
        if announce {
            self.stats_notice = true;
        }
        self.send_rpc(json!({ "type": "get_session_stats" }));
    }

    fn maybe_auto_compact(&mut self, percent: u64) {
        if percent < AUTO_COMPACT_PERCENT {
            self.auto_compacted = false;
            return;
        }
        if !should_auto_compact(percent, self.auto_compacted)
            || self.agent.busy
            || !matches!(self.pi, PiState::Running(_))
        {
            return;
        }
        self.auto_compacted = true;
        self.set_status(format!("context at {percent}%, compacting"));
        self.send_rpc(json!({ "type": "compact" }));
    }

    fn send_rpc(&mut self, command: Value) {
        match &mut self.pi {
            PiState::Running(client) => {
                if let Err(error) = client.send(command) {
                    self.agent.note(format!("pi command failed: {error}"));
                }
            }
            PiState::Missing => self.agent.note("pi is not installed"),
            PiState::Failed(error) => self.agent.note(error.clone()),
        }
    }

    fn handle_agent_command(&mut self, text: &str) {
        let Some(body) = text.strip_prefix('/') else {
            return;
        };
        self.agent.push_user(text);
        self.agent.clear_input();
        self.agent.scroll = 0;

        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let args = parts.next().unwrap_or_default().trim();

        match name {
            "help" => {
                let help = self.command_help();
                self.agent.note(help);
            }
            "new" | "reset" => self.send_rpc(json!({ "type": "new_session" })),
            "compact" => {
                let mut command = json!({ "type": "compact" });
                if !args.is_empty() {
                    command["customInstructions"] = json!(args);
                }
                self.send_rpc(command);
            }
            "abort" | "stop" => self.abort_agent(),
            "model" => self.model_command(args),
            "thinking" => self.thinking_command(args),
            "stats" | "session" => self.request_context_stats(true),
            "name" => {
                if args.is_empty() {
                    self.send_rpc(json!({ "type": "get_state" }));
                } else {
                    self.send_rpc(json!({ "type": "set_session_name", "name": args }));
                }
            }
            "export" => {
                let mut command = json!({ "type": "export_html" });
                if !args.is_empty() {
                    command["outputPath"] = json!(args);
                }
                self.send_rpc(command);
            }
            "copy" => match self.agent.last_assistant() {
                Some(text) => match copy_to_clipboard(&text) {
                    Ok(()) => self.set_status("copied"),
                    Err(error) => self.agent.note(error),
                },
                None => self.agent.note("nothing to copy"),
            },
            "hide" => self.set_agent_hidden(true),
            "show" => self.set_agent_hidden(false),
            "login" | "logout" => self.agent.note(format!(
                "/{name} runs inside pi's terminal, then restart smol"
            )),
            _ => {
                if self.commands.iter().any(|command| command.name == name) {
                    self.send_text_prompt(text);
                } else if is_pi_terminal_command(name) {
                    self.agent
                        .note(format!("/{name} runs inside pi's terminal"));
                } else {
                    self.agent
                        .note(format!("unknown command /{name}, type /help"));
                }
            }
        }
    }

    fn model_command(&mut self, args: &str) {
        if args.is_empty() {
            self.send_rpc(json!({ "type": "get_available_models" }));
            return;
        }
        if let Some((provider, id)) = args.split_once('/') {
            self.send_rpc(json!({
                "type": "set_model",
                "provider": provider,
                "modelId": id
            }));
            return;
        }

        let found = self.models.iter().find_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?;
            let name = model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let provider = model.get("provider").and_then(Value::as_str)?;
            let matches =
                id.eq_ignore_ascii_case(args) || name.to_lowercase().contains(&args.to_lowercase());
            matches.then(|| (provider.to_string(), id.to_string()))
        });

        if let Some((provider, id)) = found {
            self.send_rpc(json!({
                "type": "set_model",
                "provider": provider,
                "modelId": id
            }));
        } else {
            self.agent
                .note("use /model provider/model, or /model to list models");
        }
    }

    fn thinking_command(&mut self, args: &str) {
        const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
        if args.is_empty() {
            self.agent
                .note(format!("thinking levels  {}", LEVELS.join("  ")));
            return;
        }
        if LEVELS.contains(&args) {
            self.send_rpc(json!({ "type": "set_thinking_level", "level": args }));
        } else {
            self.agent
                .note("thinking levels  off minimal low medium high xhigh max");
        }
    }

    fn command_help(&self) -> String {
        let mut text = String::from(
            "commands\n/help  /new  /compact [instructions]  /model [provider/model]  /thinking [level]\n/abort  /stats  /name [name]  /export [path]  /copy  /hide  /show\n",
        );
        if !self.commands.is_empty() {
            text.push_str("\npi resources\n");
            for command in self.commands.iter().take(16) {
                let description = truncate(&command.description, 72);
                if description.is_empty() {
                    text.push_str(&format!("/{}\n", command.name));
                } else {
                    text.push_str(&format!("/{}  {}\n", command.name, description));
                }
            }
        }
        text.push_str(
            "\n/settings /resume /tree /fork /clone /share /login run inside pi's terminal",
        );
        text
    }

    pub fn abort_agent(&mut self) {
        if let PiState::Running(client) = &mut self.pi {
            if client.abort().is_ok() {
                self.set_status("stopping agent");
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if ctrl && key.code == KeyCode::Char('q') {
            self.should_quit = true;
            return;
        }
        if ctrl && key.code == KeyCode::Char('p') {
            self.toggle_menu();
            return;
        }
        if self.menu.is_some() {
            self.handle_menu_key(key);
            return;
        }
        if self.diff.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                self.diff = None;
                return;
            }
            if let Some(diff) = &mut self.diff {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => diff.scroll = diff.scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        diff.scroll = diff.scroll.saturating_add(1)
                    }
                    KeyCode::PageUp => diff.scroll = diff.scroll.saturating_sub(10),
                    KeyCode::PageDown => diff.scroll = diff.scroll.saturating_add(10),
                    KeyCode::Home => diff.scroll = 0,
                    KeyCode::End => diff.scroll = diff.lines.len().saturating_sub(1),
                    _ => {}
                }
            }
            return;
        }
        if key.code == KeyCode::F(6) {
            self.cycle_focus();
            return;
        }
        if ctrl && key.code == KeyCode::Char('g') {
            self.sidebar = match self.sidebar {
                Sidebar::Files => Sidebar::Git,
                Sidebar::Git => Sidebar::Files,
            };
            self.focus = Focus::Tree;
            self.git.refresh();
            return;
        }
        if ctrl && key.code == KeyCode::Char('b') {
            self.toggle_agent();
            return;
        }
        if ctrl && key.code == KeyCode::Char('s') {
            self.save();
            return;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            if self.focus == Focus::Editor {
                if let Some(text) = self.editor.selection_text() {
                    if !text.is_empty() {
                        match copy_to_clipboard(&text) {
                            Ok(()) => self.set_status("copied"),
                            Err(error) => self.set_status(error),
                        }
                    }
                }
            }
            return;
        }
        if alt {
            match key.code {
                KeyCode::Char('1') => {
                    self.focus = Focus::Tree;
                    return;
                }
                KeyCode::Char('2') => {
                    self.focus = Focus::Editor;
                    return;
                }
                KeyCode::Char('3') => {
                    self.agent_hidden = false;
                    self.focus = Focus::Agent;
                    return;
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Tree => self.handle_sidebar_key(key, ctrl, alt),
            Focus::Editor => self.handle_editor_key(key, ctrl, alt),
            Focus::Agent => self.handle_agent_key(key, ctrl, alt),
        }
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool) {
        match self.sidebar {
            Sidebar::Files => self.handle_tree_key(key),
            Sidebar::Git => self.handle_git_key(key, ctrl, alt),
        }
    }

    fn handle_git_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool) {
        if self.git.input_focus {
            match key.code {
                KeyCode::Esc => {
                    self.git.input_focus = false;
                    self.git.confirm_discard = false;
                }
                KeyCode::Enter => self.commit_git(),
                KeyCode::Backspace => self.git.backspace_input(),
                KeyCode::Delete => self.git.delete_input(),
                KeyCode::Left => self.git.move_input(-1),
                KeyCode::Right => self.git.move_input(1),
                KeyCode::Home => self.git.input_home(),
                KeyCode::End => self.git.input_end(),
                KeyCode::Char('u') if ctrl => self.git.clear_input(),
                KeyCode::Char(ch) if !ctrl && !alt => self.git.insert_input(ch),
                _ => {}
            }
            return;
        }

        if self.git.confirm_discard && !matches!(key.code, KeyCode::Char('d')) {
            self.git.confirm_discard = false;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.git.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.git.move_selection(1),
            KeyCode::Home => self.git.selected = 0,
            KeyCode::End => {
                self.git.selected = self.git.changes.len().saturating_sub(1);
            }
            KeyCode::PageUp => self.git.move_selection(-10),
            KeyCode::PageDown => self.git.move_selection(10),
            KeyCode::Enter => self.open_diff(),
            KeyCode::Char('o') => {
                if let Some(path) = self.git.selected_path() {
                    self.open_file(&path);
                }
            }
            KeyCode::Char('c') => self.git.input_focus = true,
            KeyCode::Char('p') => self.push_git(),
            KeyCode::Char('d') => self.discard_git(),
            KeyCode::Char('r') => {
                self.git.refresh();
                self.set_status("git status refreshed");
            }
            _ => {}
        }
    }

    fn commit_git(&mut self) {
        if self.git.input.trim().is_empty() {
            self.set_status("commit message is empty");
            return;
        }
        match self.git.commit() {
            Ok(message) => self.set_status(message),
            Err(error) => self.set_status(error),
        }
    }

    fn open_diff(&mut self) {
        match self.git.diff_selected() {
            Ok((title, text)) => {
                let lines = text
                    .lines()
                    .take(4000)
                    .map(|line| DiffLine {
                        kind: diff_kind(line),
                        text: line.to_string(),
                    })
                    .collect();
                self.diff = Some(DiffView {
                    title,
                    lines,
                    scroll: 0,
                });
                self.unsupported = None;
            }
            Err(error) => self.set_status(error),
        }
    }

    fn toggle_menu(&mut self) {
        if self.menu.take().is_some() {
            return;
        }
        self.menu = Some(CommandMenu {
            pages: vec![self.root_menu()],
        });
    }

    fn root_menu(&self) -> MenuPage {
        MenuPage {
            kind: None,
            title: "commands",
            selected: 0,
            items: vec![
                submenu_item("Files", 4, MenuPageKind::Files),
                submenu_item("Settings", 3, MenuPageKind::Settings),
                submenu_item("LLM commands", 6, MenuPageKind::Llm),
                submenu_item("Git", 1, MenuPageKind::Git),
                MenuItem {
                    label: "Quit".to_string(),
                    hint: "ctrl+q".to_string(),
                    action: MenuAction::Quit,
                },
            ],
        }
    }

    fn menu_page(&self, kind: MenuPageKind) -> MenuPage {
        match kind {
            MenuPageKind::Files => MenuPage {
                kind: Some(MenuPageKind::Files),
                title: "files",
                selected: 0,
                items: vec![
                    MenuItem {
                        label: "Save file".to_string(),
                        hint: "ctrl+s".to_string(),
                        action: MenuAction::Save,
                    },
                    MenuItem {
                        label: "Close file".to_string(),
                        hint: "ctrl+w".to_string(),
                        action: MenuAction::CloseFile,
                    },
                    MenuItem {
                        label: "Next file".to_string(),
                        hint: "ctrl+n".to_string(),
                        action: MenuAction::NextFile,
                    },
                    MenuItem {
                        label: "Previous file".to_string(),
                        hint: String::new(),
                        action: MenuAction::PreviousFile,
                    },
                ],
            },
            MenuPageKind::Settings => {
                let wrap = if self.editor.wrap { "on" } else { "off" };
                let agent = if self.agent_hidden { "hidden" } else { "shown" };
                let sidebar = match self.sidebar {
                    Sidebar::Files => "files",
                    Sidebar::Git => "git",
                };
                MenuPage {
                    kind: Some(MenuPageKind::Settings),
                    title: "settings",
                    selected: 0,
                    items: vec![
                        MenuItem {
                            label: "Word wrap".to_string(),
                            hint: wrap.to_string(),
                            action: MenuAction::ToggleWrap,
                        },
                        MenuItem {
                            label: "Agent pane".to_string(),
                            hint: agent.to_string(),
                            action: MenuAction::ToggleAgent,
                        },
                        MenuItem {
                            label: "Sidebar".to_string(),
                            hint: sidebar.to_string(),
                            action: MenuAction::ToggleGit,
                        },
                    ],
                }
            }
            MenuPageKind::Llm => MenuPage {
                kind: Some(MenuPageKind::Llm),
                title: "llm commands",
                selected: 0,
                items: vec![
                    MenuItem {
                        label: "Explain file".to_string(),
                        hint: "pi".to_string(),
                        action: MenuAction::ExplainFile,
                    },
                    MenuItem {
                        label: "Review file".to_string(),
                        hint: "pi".to_string(),
                        action: MenuAction::ReviewFile,
                    },
                    MenuItem {
                        label: "Explain selection".to_string(),
                        hint: "pi".to_string(),
                        action: MenuAction::ExplainSelection,
                    },
                    MenuItem {
                        label: "Refactor selection".to_string(),
                        hint: "pi".to_string(),
                        action: MenuAction::RefactorSelection,
                    },
                    MenuItem {
                        label: "Add tests".to_string(),
                        hint: "pi".to_string(),
                        action: MenuAction::AddTests,
                    },
                    MenuItem {
                        label: "Format file".to_string(),
                        hint: "pi".to_string(),
                        action: MenuAction::FormatFile,
                    },
                ],
            },
            MenuPageKind::Git => MenuPage {
                kind: Some(MenuPageKind::Git),
                title: "git",
                selected: 0,
                items: vec![MenuItem {
                    label: "Refresh git status".to_string(),
                    hint: "r".to_string(),
                    action: MenuAction::RefreshGit,
                }],
            },
        }
    }

    fn handle_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        let Some(page) = menu.pages.last_mut() else {
            self.menu = None;
            return;
        };
        match key.code {
            KeyCode::Esc => {
                let close = menu.pages.len() <= 1;
                menu.pages.pop();
                if close {
                    self.menu = None;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => page.selected = page.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                page.selected = (page.selected + 1).min(page.items.len().saturating_sub(1))
            }
            KeyCode::Home => page.selected = 0,
            KeyCode::End => page.selected = page.items.len().saturating_sub(1),
            KeyCode::Enter => {
                let action = page.items.get(page.selected).map(|item| item.action);
                if let Some(action) = action {
                    self.run_menu_command(action);
                }
            }
            _ => {}
        }
    }

    fn run_menu_command(&mut self, action: MenuAction) {
        if let MenuAction::OpenMenu(kind) = action {
            let page = self.menu_page(kind);
            if let Some(menu) = self.menu.as_mut() {
                menu.pages.push(page);
            }
            return;
        }
        let keep_open = matches!(
            action,
            MenuAction::ToggleWrap | MenuAction::ToggleAgent | MenuAction::ToggleGit
        );
        if !keep_open {
            self.menu = None;
        }
        match action {
            MenuAction::OpenMenu(_) => {}
            MenuAction::ToggleWrap => {
                let wrap = self.editor.toggle_wrap();
                self.set_status(if wrap {
                    "word wrap on"
                } else {
                    "word wrap off"
                });
            }
            MenuAction::ToggleAgent => self.toggle_agent(),
            MenuAction::ToggleGit => {
                self.sidebar = match self.sidebar {
                    Sidebar::Files => {
                        self.git.refresh();
                        Sidebar::Git
                    }
                    Sidebar::Git => Sidebar::Files,
                };
                self.focus = Focus::Tree;
            }
            MenuAction::Save => self.save(),
            MenuAction::CloseFile => match self.editor.close_active() {
                Ok(message) => self.set_status(message),
                Err(error) => self.set_status(error),
            },
            MenuAction::NextFile => self.editor.switch_buffer(1),
            MenuAction::PreviousFile => self.editor.switch_buffer(-1),
            MenuAction::RefreshGit => {
                self.git.refresh();
                self.set_status("git status refreshed");
            }
            MenuAction::ExplainFile => self.explain_file_with_pi(),
            MenuAction::ReviewFile => self.review_file_with_pi(),
            MenuAction::ExplainSelection => self.explain_selection_with_pi(),
            MenuAction::RefactorSelection => self.refactor_selection_with_pi(),
            MenuAction::AddTests => self.add_tests_with_pi(),
            MenuAction::FormatFile => self.format_file_with_pi(),
            MenuAction::Quit => self.should_quit = true,
        }
        if keep_open {
            self.refresh_menu_page();
        }
    }

    fn refresh_menu_page(&mut self) {
        let Some(page) = self.menu.as_ref().and_then(|menu| menu.pages.last()) else {
            return;
        };
        let Some(kind) = page.kind else {
            return;
        };
        let selected = page.selected;
        let mut refreshed = self.menu_page(kind);
        refreshed.selected = selected.min(refreshed.items.len().saturating_sub(1));
        if let Some(page) = self.menu.as_mut().and_then(|menu| menu.pages.last_mut()) {
            *page = refreshed;
        }
    }

    fn ensure_pi(&mut self) -> bool {
        match &self.pi {
            PiState::Missing => {
                self.set_status("pi is not installed");
                false
            }
            PiState::Failed(error) => {
                let error = error.clone();
                self.set_status(error);
                false
            }
            PiState::Running(_) => true,
        }
    }

    fn saved_file_for_pi(&mut self, action: &str) -> Option<String> {
        let Some(buffer) = self.editor.buffer() else {
            self.set_status("no file open");
            return None;
        };
        let rel = buffer.rel.clone();
        if buffer.dirty {
            self.set_status(format!("save the file before {action}"));
            return None;
        }
        self.ensure_pi().then_some(rel)
    }

    fn selection_for_pi(&mut self) -> Option<(String, String)> {
        let Some(buffer) = self.editor.buffer() else {
            self.set_status("no file open");
            return None;
        };
        let rel = buffer.rel.clone();
        let Some(text) = self.editor.selection_text() else {
            self.set_status("select some code first");
            return None;
        };
        if text.trim().is_empty() {
            self.set_status("select some code first");
            return None;
        }
        if text.len() > 20_000 {
            self.set_status("selection is too large for an LLM command");
            return None;
        }
        Some((rel, text))
    }

    fn start_llm_prompt(&mut self, prompt: String, status: String) {
        self.agent_hidden = false;
        self.focus = Focus::Agent;
        self.agent.push_user(&prompt);
        self.send_text_prompt(&prompt);
        self.set_status(status);
    }

    fn explain_file_with_pi(&mut self) {
        let Some(rel) = self.saved_file_for_pi("explaining") else {
            return;
        };
        let prompt = format!(
            "Explain {rel}. Cover its purpose, structure, important control flow, and anything a new contributor should know. Do not edit files."
        );
        self.start_llm_prompt(prompt, format!("explaining {rel} with pi"));
    }

    fn review_file_with_pi(&mut self) {
        let Some(rel) = self.saved_file_for_pi("reviewing") else {
            return;
        };
        let prompt = format!(
            "Review {rel} for bugs, regressions, security issues, and unclear code. Do not edit files. Report concrete findings with line references, ordered by severity."
        );
        self.start_llm_prompt(prompt, format!("reviewing {rel} with pi"));
    }

    fn explain_selection_with_pi(&mut self) {
        let Some((rel, text)) = self.selection_for_pi() else {
            return;
        };
        if !self.ensure_pi() {
            return;
        }
        let prompt = format!(
            "Explain this code selected from {rel}. Focus on what it does, its inputs and side effects, and any risks. Do not edit files.\n\n<selection>\n{text}\n</selection>"
        );
        self.start_llm_prompt(prompt, format!("explaining selection in {rel} with pi"));
    }

    fn refactor_selection_with_pi(&mut self) {
        let Some((rel, text)) = self.selection_for_pi() else {
            return;
        };
        if self.editor.buffer().is_some_and(|buffer| buffer.dirty) {
            self.set_status("save the file before refactoring a selection");
            return;
        }
        if !self.ensure_pi() {
            return;
        }
        let prompt = format!(
            "Refactor only this code selected from {rel}. Preserve its behavior, follow the project's style, edit the file in place, and summarize the change. Do not modify unrelated code.\n\n<selection>\n{text}\n</selection>"
        );
        self.start_llm_prompt(prompt, format!("refactoring selection in {rel} with pi"));
    }

    fn add_tests_with_pi(&mut self) {
        let Some(rel) = self.saved_file_for_pi("adding tests") else {
            return;
        };
        let prompt = format!(
            "Add focused tests for {rel}. Follow the project's existing test conventions, cover the main behavior and important edge cases, run the relevant tests, and fix any failures you introduced. Summarize the tests added."
        );
        self.start_llm_prompt(prompt, format!("adding tests for {rel} with pi"));
    }

    fn format_file_with_pi(&mut self) {
        let Some(rel) = self.saved_file_for_pi("formatting") else {
            return;
        };
        let prompt = format!(
            "Format {rel} for an LLM. Keep its behavior unchanged, improve the structure and whitespace for an LLM to read, edit the file in place, then summarize the changes."
        );
        self.start_llm_prompt(prompt, format!("formatting {rel} with pi"));
    }

    fn push_git(&mut self) {
        match self.git.push() {
            Ok(()) => self.set_status("pushing"),
            Err(error) => self.set_status(error),
        }
    }

    fn discard_git(&mut self) {
        let Some(change) = self.git.selected_change().cloned() else {
            return;
        };
        if !self.git.confirm_discard {
            self.git.confirm_discard = true;
            self.set_status(format!("press d again to discard {}", change.path));
            return;
        }
        match self.git.discard_selected() {
            Ok((message, path)) => {
                self.set_status(message);
                self.reload_after_tool(&path);
                self.tree.refresh();
            }
            Err(error) => self.set_status(error),
        }
    }

    fn handle_tree_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.tree.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.tree.move_selection(1),
            KeyCode::Left | KeyCode::Char('h') => self.tree.collapse_selected(),
            KeyCode::Right | KeyCode::Char('l') => self.tree.expand_selected(),
            KeyCode::Enter => {
                if let Some(path) = self.tree.activate() {
                    self.open_file(&path);
                }
            }
            KeyCode::Home => self.tree.select_first(),
            KeyCode::End => self.tree.select_last(),
            KeyCode::PageUp => self.tree.move_selection(-10),
            KeyCode::PageDown => self.tree.move_selection(10),
            _ => {}
        }
    }

    fn handle_editor_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool) {
        if self.unsupported.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('o') => self.open_unsupported_in_default(),
                KeyCode::Esc => {
                    self.unsupported = None;
                    self.set_status("closed unsupported file view");
                }
                _ => {}
            }
            return;
        }

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter => self.editor.newline(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left if ctrl => self.editor.move_word_left(shift),
            KeyCode::Right if ctrl => self.editor.move_word_right(shift),
            KeyCode::Left => self.editor.move_left(shift),
            KeyCode::Right => self.editor.move_right(shift),
            KeyCode::Up => self.editor.move_up(shift),
            KeyCode::Down => self.editor.move_down(shift),
            KeyCode::Home if ctrl => self.editor.doc_start(shift),
            KeyCode::End if ctrl => self.editor.doc_end(shift),
            KeyCode::Home => self.editor.move_home(shift),
            KeyCode::End => self.editor.move_end(shift),
            KeyCode::PageUp => self.editor.move_page(-15, shift),
            KeyCode::PageDown => self.editor.move_page(15, shift),
            KeyCode::Tab if self.editor.ghost.is_some() => {
                self.editor.accept_prediction();
            }
            KeyCode::Tab => self.editor.indent(),
            KeyCode::BackTab => self.editor.dedent(),
            KeyCode::Char('z') if ctrl => {
                if self.editor.undo() {
                    self.set_status("undid edit");
                } else {
                    self.set_status("nothing to undo");
                }
            }
            KeyCode::Char('n') if ctrl => self.editor.switch_buffer(1),
            KeyCode::Char('w') if ctrl => match self.editor.close_active() {
                Ok(message) => self.set_status(message),
                Err(error) => self.set_status(error),
            },
            KeyCode::Char(ch) if !ctrl && !alt => self.editor.insert_char(ch),
            _ => {}
        }
    }

    fn handle_agent_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool) {
        let running = matches!(self.pi, PiState::Running(_));
        match key.code {
            KeyCode::PageUp => self.agent.scroll_by(5),
            KeyCode::PageDown => self.agent.scroll_by(-5),
            KeyCode::Esc => {
                if self.agent.busy {
                    self.abort_agent();
                }
            }
            KeyCode::Enter => self.send_prompt(),
            _ if !running => {}
            KeyCode::Backspace => self.agent.backspace_input(),
            KeyCode::Delete => self.agent.delete_input(),
            KeyCode::Left => self.agent.move_input(-1),
            KeyCode::Right => self.agent.move_input(1),
            KeyCode::Home => self.agent.input_home(),
            KeyCode::End => self.agent.input_end(),
            KeyCode::Char('u') if ctrl => self.agent.clear_input(),
            KeyCode::Char(ch) if !ctrl && !alt => self.agent.insert_input(ch),
            _ => {}
        }
    }

    fn save(&mut self) {
        if self.unsupported.is_some() {
            self.set_status("this file type is not supported");
            return;
        }
        match self.editor.save_active() {
            Ok(message) => self.set_status(message),
            Err(error) => self.set_status(error),
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        self.open_file(path);
    }

    pub fn status_hint(&self, width: u16) -> String {
        let full = width >= 110;
        if let Some(menu) = &self.menu {
            return if menu.pages.len() > 1 {
                "enter run  esc back"
            } else {
                "enter open  esc close"
            }
            .to_string();
        }
        if self.diff.is_some() {
            return "j/k scroll  esc close".to_string();
        }
        if self.unsupported.is_some() {
            return if full {
                "enter open in default app  esc close"
            } else {
                "enter open  esc close"
            }
            .to_string();
        }

        let sidebar_toggle = if self.sidebar == Sidebar::Git {
            "ctrl+g files"
        } else {
            "ctrl+g git"
        };
        let agent_toggle = if self.agent_hidden {
            "ctrl+b show"
        } else {
            "ctrl+b agent"
        };

        if self.focus == Focus::Tree {
            return match self.sidebar {
                Sidebar::Git if self.git.input_focus => {
                    if full {
                        "enter commit  esc cancel"
                    } else {
                        "enter commit"
                    }
                }
                Sidebar::Git => {
                    if full {
                        "enter diff  o open  c commit  p push  d discard  r refresh  ctrl+g files"
                    } else {
                        "enter diff  c commit  p push  d discard  ctrl+g files"
                    }
                }
                Sidebar::Files => {
                    if full {
                        return format!(
                            "enter open  h/l fold  {sidebar_toggle}  {agent_toggle}  ctrl+q quit"
                        );
                    }
                    return format!("enter open  {sidebar_toggle}  ctrl+q quit");
                }
            }
            .to_string();
        }

        if self.focus == Focus::Agent {
            return match &self.pi {
                PiState::Running(_) if self.agent.busy => {
                    if full {
                        "enter queue  esc stop  ctrl+b hide"
                    } else {
                        "esc stop  ctrl+b hide"
                    }
                }
                PiState::Running(_) => {
                    if full {
                        "enter send  /help commands  ctrl+b hide"
                    } else {
                        "enter send  ctrl+b hide"
                    }
                }
                PiState::Missing | PiState::Failed(_) => {
                    if full {
                        "pi unavailable  ctrl+b hide  ctrl+g files"
                    } else {
                        "ctrl+b hide"
                    }
                }
            }
            .to_string();
        }

        match self.editor.buffer() {
            Some(buffer) if buffer.dirty => {
                if full {
                    format!(
                        "ctrl+s save  ctrl+z undo  ctrl+n tab  ctrl+w close  ctrl+p menu  {sidebar_toggle}"
                    )
                } else {
                    format!("ctrl+s save  ctrl+p menu  {sidebar_toggle}")
                }
            }
            Some(_) => {
                if full {
                    format!(
                        "ctrl+z undo  ctrl+n tab  ctrl+w close  ctrl+p menu  {sidebar_toggle}  {agent_toggle}"
                    )
                } else {
                    format!("ctrl+z undo  ctrl+p menu  {sidebar_toggle}  ctrl+q quit")
                }
            }
            None => {
                if full {
                    format!(
                        "select a file  ctrl+p menu  {sidebar_toggle}  {agent_toggle}  ctrl+q quit"
                    )
                } else {
                    format!("ctrl+p menu  {sidebar_toggle}  ctrl+q quit")
                }
            }
        }
    }

    pub fn toggle_agent(&mut self) {
        self.set_agent_hidden(!self.agent_hidden);
    }

    pub fn set_agent_hidden(&mut self, hidden: bool) {
        self.agent_hidden = hidden;
        if hidden && self.focus == Focus::Agent {
            self.focus = Focus::Editor;
        }
    }

    fn open_file(&mut self, path: &Path) {
        self.diff = None;
        if is_media_file(path) {
            self.show_unsupported(path);
            return;
        }
        match self.editor.open(path) {
            Ok(()) => {
                self.unsupported = None;
                self.focus = Focus::Editor;
                self.tree.reveal(path);
            }
            Err(error) if is_unsupported_error(&error) => self.show_unsupported(path),
            Err(error) => self.set_status(error),
        }
    }

    fn show_unsupported(&mut self, path: &Path) {
        self.unsupported = Some(path.to_path_buf());
        self.focus = Focus::Editor;
        self.tree.reveal(path);
    }

    fn open_unsupported_in_default(&mut self) {
        let Some(path) = self.unsupported.clone() else {
            return;
        };
        match open_in_default_app(&path) {
            Ok(()) => self.set_status("opened in default app"),
            Err(error) => self.set_status(error),
        }
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Editor,
            Focus::Editor if self.agent_hidden => Focus::Tree,
            Focus::Editor => Focus::Agent,
            Focus::Agent => Focus::Tree,
        };
    }

    pub fn handle_paste(&mut self, text: &str) {
        match self.focus {
            Focus::Editor => self.editor.insert_text(text),
            Focus::Agent => {
                let text = text.replace(['\r', '\n'], " ");
                for ch in text.chars() {
                    self.agent.insert_input(ch);
                }
            }
            Focus::Tree => {}
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let x = mouse.column;
        let y = mouse.row;
        self.hover = Some((x, y));
        if self.menu.is_some() {
            return;
        }
        self.refresh_hover();

        if self.diff.is_some() {
            let in_editor = inside(self.ui.editor_text, x, y);
            match mouse.kind {
                MouseEventKind::ScrollUp if in_editor => {
                    if let Some(diff) = &mut self.diff {
                        diff.scroll = diff.scroll.saturating_sub(3);
                    }
                    return;
                }
                MouseEventKind::ScrollDown if in_editor => {
                    if let Some(diff) = &mut self.diff {
                        diff.scroll = diff.scroll.saturating_add(3);
                    }
                    return;
                }
                MouseEventKind::Down(MouseButton::Left) if inside(self.ui.tree, x, y) => {}
                _ => return,
            }
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.git.confirm_discard = false;
                self.dragging = false;
                if let Some(edge) = self.resize_edge_at(x, y) {
                    self.resizing = Some(edge);
                    self.hover_edge = Some(edge);
                    self.set_pointer(PointerShape::Resize);
                    return;
                }
                if inside(self.ui.tree, x, y) {
                    self.focus = Focus::Tree;
                    if self.sidebar == Sidebar::Git {
                        if inside(self.ui.git_input, x, y) {
                            self.git.input_focus = true;
                        } else if y >= self.ui.tree_body.y {
                            if self.git_hover_row(x, y).is_some() {
                                self.git
                                    .select_row(y.saturating_sub(self.ui.tree_body.y) as usize);
                                self.open_diff();
                            }
                        }
                    } else if y >= self.ui.tree_body.y {
                        let row = y.saturating_sub(self.ui.tree_body.y) as usize;
                        self.tree.select_row(row);
                        if let Some(path) = self.tree.activate() {
                            self.open_file(&path);
                        }
                    }
                } else if inside(self.ui.editor, x, y) {
                    self.focus = Focus::Editor;
                    if inside(self.ui.unsupported_button, x, y) {
                        self.open_unsupported_in_default();
                        return;
                    }
                    let tab = self
                        .ui
                        .tabs
                        .iter()
                        .find(|(rect, _)| inside(*rect, x, y))
                        .map(|(_, index)| *index);
                    if let Some(index) = tab {
                        self.editor.select_buffer(index);
                    } else if inside(self.ui.editor_text, x, y) {
                        let (row, cells, width) = self.editor_position(x, y);
                        self.editor.begin_mouse_selection(row, cells, width);
                        self.dragging = true;
                    }
                } else if inside(self.ui.agent, x, y) {
                    self.focus = Focus::Agent;
                    if let Some(index) = self.thinking_hover_entry(x, y) {
                        if self.agent.toggle_thinking(index) {
                            return;
                        }
                    }
                    if inside(self.ui.agent_input, x, y) {
                        self.agent.input_end();
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(edge) = self.resizing {
                    self.resize_drag(edge, x);
                    return;
                }
                if self.dragging && inside(self.ui.editor_text, x, y) {
                    let (row, cells, width) = self.editor_position(x, y);
                    self.editor.extend_mouse_selection(row, cells, width);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = false;
                self.resizing = None;
                self.refresh_hover();
            }
            MouseEventKind::ScrollUp => {
                if inside(self.ui.tree, x, y) {
                    match self.sidebar {
                        Sidebar::Files => self.tree.move_selection(-1),
                        Sidebar::Git => self.git.move_selection(-1),
                    }
                } else if inside(self.ui.editor, x, y) {
                    let width = self.ui.editor_text.width.saturating_sub(self.ui.gutter) as usize;
                    self.editor.scroll_by(-3, width);
                } else if inside(self.ui.agent, x, y) {
                    self.agent.scroll_by(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if inside(self.ui.tree, x, y) {
                    match self.sidebar {
                        Sidebar::Files => self.tree.move_selection(1),
                        Sidebar::Git => self.git.move_selection(1),
                    }
                } else if inside(self.ui.editor, x, y) {
                    let width = self.ui.editor_text.width.saturating_sub(self.ui.gutter) as usize;
                    self.editor.scroll_by(3, width);
                } else if inside(self.ui.agent, x, y) {
                    self.agent.scroll_by(-3);
                }
            }
            _ => {}
        }
    }
}

impl App {
    #[cfg(test)]
    pub fn pointer(&self) -> PointerShape {
        self.pointer
    }

    pub fn take_pointer_update(&mut self) -> Option<PointerShape> {
        if self.pointer_dirty {
            self.pointer_dirty = false;
            Some(self.pointer)
        } else {
            None
        }
    }

    pub fn refresh_hover(&mut self) {
        let Some((x, y)) = self.hover else {
            self.hover_edge = None;
            self.set_pointer(PointerShape::Default);
            return;
        };

        if self.diff.is_some() {
            self.hover_edge = None;
            let over_row = match self.sidebar {
                Sidebar::Files => self.tree_hover_row(x, y).is_some(),
                Sidebar::Git => self.git_hover_row(x, y).is_some(),
            };
            if over_row {
                self.set_pointer(PointerShape::Pointer);
            } else {
                self.set_pointer(PointerShape::Default);
            }
            return;
        }

        let edge = self.resize_edge_at(x, y);
        self.hover_edge = edge;
        if edge.is_some() || self.resizing.is_some() {
            self.set_pointer(PointerShape::Resize);
            return;
        }

        if inside(self.ui.tree, x, y) {
            if self.sidebar == Sidebar::Git && inside(self.ui.git_input, x, y) {
                self.set_pointer(PointerShape::Text);
            } else if self.sidebar == Sidebar::Files && self.tree_hover_row(x, y).is_some() {
                self.set_pointer(PointerShape::Pointer);
            } else if self.sidebar == Sidebar::Git && self.git_hover_row(x, y).is_some() {
                self.set_pointer(PointerShape::Pointer);
            } else {
                self.set_pointer(PointerShape::Default);
            }
            return;
        }

        if inside(self.ui.unsupported_button, x, y)
            || self.ui.tabs.iter().any(|(rect, _)| inside(*rect, x, y))
            || self.thinking_hover_entry(x, y).is_some()
        {
            self.set_pointer(PointerShape::Pointer);
        } else if inside(self.ui.editor_text, x, y) || inside(self.ui.agent_input, x, y) {
            self.set_pointer(PointerShape::Text);
        } else {
            self.set_pointer(PointerShape::Default);
        }
    }

    fn set_pointer(&mut self, shape: PointerShape) {
        if self.pointer != shape {
            self.pointer = shape;
            self.pointer_dirty = true;
        }
    }

    fn tree_hover_row(&self, x: u16, y: u16) -> Option<usize> {
        if !inside(self.ui.tree_body, x, y) {
            return None;
        }
        let row = (y - self.ui.tree_body.y) as usize + self.tree.scroll;
        (row < self.tree.visible.len()).then_some(row)
    }

    fn git_hover_row(&self, x: u16, y: u16) -> Option<usize> {
        if !inside(self.ui.tree_body, x, y) || !self.git.repo || self.git.changes.is_empty() {
            return None;
        }
        let row = (y - self.ui.tree_body.y) as usize + self.git.scroll;
        (row < self.git.changes.len()).then_some(row)
    }

    fn thinking_hover_entry(&self, x: u16, y: u16) -> Option<usize> {
        self.ui
            .agent_thinking
            .iter()
            .find(|(rect, _)| inside(*rect, x, y))
            .map(|(_, index)| *index)
    }

    fn resize_edge_at(&self, x: u16, y: u16) -> Option<ResizeEdge> {
        if !inside(self.ui.main, x, y) {
            return None;
        }
        if let Some(divider) = self.ui.tree_divider {
            if x.abs_diff(divider) <= 1 {
                return Some(ResizeEdge::Sidebar);
            }
        }
        if let Some(divider) = self.ui.agent_divider {
            if x.abs_diff(divider) <= 1 {
                return Some(ResizeEdge::Agent);
            }
        }
        None
    }

    fn resize_drag(&mut self, edge: ResizeEdge, x: u16) {
        let main = self.ui.main;
        let min_left = 12u16;
        let min_editor = 16u16;
        let min_agent = 18u16;

        match edge {
            ResizeEdge::Sidebar => {
                let right = if self.agent_hidden {
                    0
                } else {
                    self.right_width.unwrap_or(24)
                };
                let max = main
                    .width
                    .saturating_sub(min_editor.saturating_add(right))
                    .max(min_left);
                let target = x
                    .saturating_sub(main.x)
                    .saturating_add(1)
                    .clamp(min_left.min(max), max);
                self.left_width = Some(target);
            }
            ResizeEdge::Agent => {
                let left = self.left_width.unwrap_or(16);
                let max = main
                    .width
                    .saturating_sub(min_editor.saturating_add(left))
                    .max(min_agent);
                let target = main
                    .x
                    .saturating_add(main.width)
                    .saturating_sub(x)
                    .clamp(min_agent.min(max), max);
                self.right_width = Some(target);
            }
        }
    }

    fn editor_position(&self, x: u16, y: u16) -> (usize, usize, usize) {
        let row = self
            .editor
            .buffer()
            .map(|buffer| buffer.scroll + y.saturating_sub(self.ui.editor_text.y) as usize)
            .unwrap_or(0);
        let cells = x.saturating_sub(self.ui.editor_text.x + self.ui.gutter) as usize;
        let width = self.ui.editor_text.width.saturating_sub(self.ui.gutter) as usize;
        (row, cells, width)
    }
}

fn inside(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn is_media_file(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "ico"
            | "tif"
            | "tiff"
            | "avif"
            | "heic"
            | "svg"
            | "mp3"
            | "wav"
            | "flac"
            | "ogg"
            | "m4a"
            | "aac"
            | "opus"
            | "wma"
            | "aiff"
            | "aif"
            | "mp4"
            | "mov"
            | "mkv"
            | "webm"
            | "avi"
            | "pdf"
            | "zip"
            | "gz"
            | "tgz"
            | "xz"
            | "7z"
            | "rar"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "so"
            | "dll"
            | "dylib"
            | "bin"
            | "exe"
            | "class"
            | "jar"
            | "wasm"
    )
}

fn is_unsupported_error(error: &str) -> bool {
    error.contains("binary file") || error.contains("not utf-8") || error.contains("too large")
}

#[cfg(target_os = "windows")]
fn open_in_default_app(path: &Path) -> Result<(), String> {
    Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open default app: {error}"))
}

#[cfg(target_os = "macos")]
fn open_in_default_app(path: &Path) -> Result<(), String> {
    Command::new("open")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open default app: {error}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_in_default_app(path: &Path) -> Result<(), String> {
    for (program, open) in [("xdg-open", false), ("gio", true)] {
        let mut command = Command::new(program);
        if open {
            command.arg("open");
        }
        command
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if command.spawn().is_ok() {
            return Ok(());
        }
    }
    Err("could not find a default app opener".to_string())
}

fn is_pi_terminal_command(name: &str) -> bool {
    matches!(
        name,
        "settings"
            | "resume"
            | "tree"
            | "fork"
            | "clone"
            | "share"
            | "bug"
            | "trust"
            | "reload"
            | "hotkeys"
            | "changelog"
            | "import"
            | "scoped-models"
            | "quit"
    )
}

fn tool_path(args: &Value) -> Option<String> {
    for key in ["path", "file_path", "file"] {
        if let Some(path) = args.get(key).and_then(Value::as_str) {
            return Some(path.to_string());
        }
    }
    None
}

fn tool_label(name: &str, args: &Value, path: Option<&Path>, root: &Path) -> String {
    let display = path
        .map(|path| display_path(path, root))
        .or_else(|| tool_path(args));

    match name {
        "read" => {
            let mut label = format!("read {}", display.unwrap_or_default());
            if let Some(offset) = args.get("offset").and_then(Value::as_u64) {
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(200);
                label.push_str(&format!(" {}-{}", offset.max(1), offset.max(1) + limit));
            }
            label
        }
        "write" => format!("write {}", display.unwrap_or_default()),
        "edit" => format!("edit {}", display.unwrap_or_default()),
        "bash" => {
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default();
            format!("bash {}", truncate(command, 48))
        }
        "grep" => {
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("grep {pattern}")
        }
        "find" => {
            let pattern = args
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default();
            format!("find {pattern}")
        }
        "ls" => format!("ls {}", display.unwrap_or_default()),
        "todo" => "todo".to_string(),
        other => match display {
            Some(path) => format!("{other} {path}"),
            None => other.to_string(),
        },
    }
}

fn display_path(path: &Path, root: &Path) -> String {
    let relative = path
        .strip_prefix(root)
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

fn truncate(text: &str, width: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= width {
            out.push_str("..");
            break;
        }
        out.push(ch);
    }
    out
}

fn should_auto_compact(percent: u64, compacted: bool) -> bool {
    percent >= AUTO_COMPACT_PERCENT && !compacted
}

fn read_range(args: &Value, content: Option<&str>) -> (usize, usize) {
    let total = content
        .map(|content| content.lines().count())
        .unwrap_or(0)
        .max(1);
    let start = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let start = start.min(total);
    let end = match args.get("limit").and_then(Value::as_u64) {
        Some(limit) if limit > 0 => start.saturating_add(limit as usize).saturating_sub(1),
        _ => start.saturating_add(199),
    };
    (start, end.min(total).max(start))
}

fn changed_range(before: Option<&str>, after: Option<&str>) -> Option<(usize, usize)> {
    let before: Vec<&str> = before.unwrap_or_default().lines().collect();
    let after: Vec<&str> = after.unwrap_or_default().lines().collect();
    if before == after {
        return None;
    }
    if after.is_empty() {
        return Some((1, 1));
    }

    let mut start = 0;
    while start < before.len() && start < after.len() && before[start] == after[start] {
        start += 1;
    }
    let mut before_end = before.len();
    let mut after_end = after.len();
    while before_end > start && after_end > start && before[before_end - 1] == after[after_end - 1]
    {
        before_end -= 1;
        after_end -= 1;
    }

    Some((start + 1, after_end.max(start + 1)))
}

fn diff_kind(line: &str) -> DiffKind {
    if line.starts_with("+++") || line.starts_with("---") {
        DiffKind::Meta
    } else if line.starts_with("@@") {
        DiffKind::Hunk
    } else if line.starts_with('+') {
        DiffKind::Add
    } else if line.starts_with('-') {
        DiffKind::Del
    } else {
        DiffKind::Context
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if text.len() > 100_000 {
        return Err("selection is too large to copy".into());
    }
    let encoded = base64(text.as_bytes());
    let mut stdout = io::stdout();
    stdout
        .write_all(b"\x1b]52;c;")
        .and_then(|_| stdout.write_all(encoded.as_bytes()))
        .and_then(|_| stdout.write_all(b"\x07"))
        .and_then(|_| stdout.flush())
        .map_err(|error| format!("copy failed: {error}"))?;
    Ok(())
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (first << 16) | (second << 8) | third;
        out.push(TABLE[((value >> 18) & 63) as usize] as char);
        out.push(TABLE[((value >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((value >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(value & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    block.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true)
    }

    #[test]
    fn auto_compaction_uses_95_percent_threshold() {
        assert!(!should_auto_compact(94, false));
        assert!(should_auto_compact(95, false));
        assert!(should_auto_compact(100, false));
        assert!(!should_auto_compact(95, true));
    }

    #[test]
    fn session_stats_store_context_percent() {
        let mut app = test_app();
        app.on_response(&json!({
            "type": "response",
            "command": "get_session_stats",
            "success": true,
            "data": {
                "totalMessages": 3,
                "tokens": { "total": 1200 },
                "contextUsage": { "percent": 95 }
            }
        }));
        assert_eq!(app.context_percent, Some(95));
    }

    #[test]
    fn thinking_blocks_share_one_assistant_entry() {
        let mut app = test_app();
        app.on_pi_record(json!({
            "type": "message_start",
            "message": { "role": "assistant" }
        }));
        app.on_pi_record(json!({
            "type": "message_update",
            "assistantMessageEvent": { "type": "thinking_delta", "delta": "first " }
        }));
        app.on_pi_record(json!({
            "type": "message_end",
            "message": { "role": "assistant", "content": [] }
        }));
        app.on_pi_record(json!({
            "type": "message_start",
            "message": { "role": "assistant" }
        }));
        app.on_pi_record(json!({
            "type": "message_update",
            "assistantMessageEvent": { "type": "thinking_delta", "delta": "second" }
        }));
        app.on_pi_record(json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "answer" }]
            }
        }));

        let assistants = app
            .agent
            .entries
            .iter()
            .filter(|entry| matches!(entry, Entry::Assistant { .. }))
            .count();
        assert_eq!(assistants, 1);
        match app.agent.entries.last() {
            Some(Entry::Assistant {
                text,
                thinking,
                thinking_open,
            }) => {
                assert_eq!(text, "answer");
                assert_eq!(thinking, "first second");
                assert!(!thinking_open);
            }
            _ => panic!("assistant entry missing"),
        }
    }

    #[test]
    fn thinking_stream_is_kept_with_assistant_message() {
        let mut app = test_app();
        app.on_pi_record(json!({
            "type": "message_start",
            "message": { "role": "assistant" }
        }));
        app.on_pi_record(json!({
            "type": "message_update",
            "assistantMessageEvent": { "type": "thinking_delta", "delta": "first " }
        }));
        app.on_pi_record(json!({
            "type": "message_update",
            "assistantMessageEvent": { "type": "thinking_delta", "delta": "second" }
        }));
        app.on_pi_record(json!({
            "type": "message_update",
            "assistantMessageEvent": {
                "type": "thinking_end",
                "content": "first second"
            }
        }));
        match app.agent.entries.last() {
            Some(Entry::Assistant {
                thinking,
                thinking_open,
                ..
            }) => {
                assert_eq!(thinking, "first second");
                assert!(!thinking_open);
            }
            _ => panic!("assistant entry missing"),
        }
        app.on_pi_record(json!({
            "type": "message_update",
            "assistantMessageEvent": { "type": "text_delta", "delta": "answer" }
        }));
        app.on_pi_record(json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "answer" }]
            }
        }));

        match app.agent.entries.last() {
            Some(Entry::Assistant {
                text,
                thinking,
                thinking_open,
            }) => {
                assert_eq!(text, "answer");
                assert_eq!(thinking, "first second");
                assert!(!thinking_open);
            }
            _ => panic!("assistant entry missing"),
        }
    }

    #[test]
    fn tracks_read_ranges() {
        let mut app = test_app();
        app.on_pi_record(json!({
            "type": "tool_execution_start",
            "toolCallId": "call-1",
            "toolName": "read",
            "args": { "path": "README.md", "offset": 2, "limit": 3 }
        }));

        let track = app
            .tracks
            .iter()
            .find(|track| track.path.ends_with("README.md"))
            .expect("read track");
        assert_eq!(track.start, 2);
        assert_eq!(track.end, 4);
        assert!(track.active);

        app.on_pi_record(json!({
            "type": "tool_execution_end",
            "toolCallId": "call-1",
            "toolName": "read",
            "isError": false
        }));
        assert!(
            !app.tracks
                .iter()
                .find(|track| track.path.ends_with("README.md"))
                .expect("read track")
                .active
        );
    }

    #[test]
    fn undo_key_restores_editor() {
        let mut app = test_app();
        app.open_path(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"));
        let before = app.editor.buffer().unwrap().lines[0].clone();
        app.editor.insert_char('X');
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.editor.buffer().unwrap().lines[0], before);
    }

    #[test]
    fn status_hints_follow_context() {
        let mut app = test_app();
        app.open_path(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"));
        app.focus = Focus::Editor;
        assert!(app.status_hint(120).contains("ctrl+z undo"));
        app.sidebar = Sidebar::Git;
        app.focus = Focus::Tree;
        assert!(app.status_hint(120).contains("discard"));
        assert!(app.status_hint(120).contains("ctrl+g files"));
        app.focus = Focus::Editor;
        assert!(app.status_hint(120).contains("ctrl+g files"));
        app.focus = Focus::Agent;
        assert!(app.status_hint(120).contains("ctrl+b hide"));
    }

    #[test]
    fn recognizes_media_and_binary_errors() {
        assert!(is_media_file(Path::new("photo.PNG")));
        assert!(is_media_file(Path::new("song.flac")));
        assert!(!is_media_file(Path::new("src/main.rs")));
        assert!(is_unsupported_error("binary file"));
        assert!(is_unsupported_error("file is not utf-8"));
        assert!(!is_unsupported_error("no such file"));
    }

    #[test]
    fn local_commands_work_without_pi() {
        let mut app = test_app();
        app.agent.input = "/help".to_string();
        app.send_prompt();
        assert!(matches!(
            app.agent.entries.last(),
            Some(Entry::Note(note)) if note.contains("commands")
        ));

        app.agent.input = "/hide".to_string();
        app.send_prompt();
        assert!(app.agent_hidden);
        app.set_agent_hidden(false);
        assert!(!app.agent_hidden);
    }
}
