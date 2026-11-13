use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use serde_json::{json, Value};

use crate::editor::Editor;
use crate::git::Git;
use crate::pi::{find_pi, PiClient, PiEvent};
use crate::todo;
use crate::tree::FileTree;

const AUTO_COMPACT_PERCENT: u64 = 95;

/// Every level pi accepts; the picker narrows this to what the model supports
/// once `get_available_thinking_levels` answers.
const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

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
    pub diff_header: Rect,
    pub status: Rect,
    pub context_menu: Rect,
    pub history: Rect,
    pub history_rows: Vec<(Rect, usize)>,
    pub picker: Rect,
    pub picker_rows: Vec<(Rect, usize)>,
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
    pub body: Vec<ToolBodyLine>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolBodyKind {
    Text,
    Add,
    Del,
    Meta,
}

pub struct ToolBodyLine {
    pub kind: ToolBodyKind,
    pub text: String,
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
    PushGit,
    ExplainFile,
    ReviewFile,
    ExplainSelection,
    RefactorSelection,
    AddTests,
    FormatFile,
    SessionHistory,
    ModelPicker,
    Thinking,
    Provider,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContextAction {
    Open,
    OpenDefault,
    NewFile,
    NewFolder,
    Rename,
    Delete,
    CopyPath,
    CopyRelativePath,
}

pub struct ContextMenuItem {
    pub label: &'static str,
    pub hint: &'static str,
    pub action: ContextAction,
}

pub struct ContextMenu {
    pub target: PathBuf,
    pub is_dir: bool,
    pub selected: usize,
    pub anchor: (u16, u16),
    pub items: Vec<ContextMenuItem>,
}

#[derive(Clone)]
pub struct TextPrompt {
    pub title: String,
    pub value: String,
    pub cursor: usize,
}

impl TextPrompt {
    fn new(title: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self {
            title: title.into(),
            value,
            cursor,
        }
    }

    fn insert(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        let mut chars: Vec<char> = self.value.chars().collect();
        chars.insert(self.cursor.min(chars.len()), ch);
        self.cursor = (self.cursor + 1).min(chars.len());
        self.value = chars.into_iter().collect();
    }

    fn insert_text(&mut self, text: &str) {
        for ch in text.chars().filter(|ch| !ch.is_control()) {
            self.insert(ch);
        }
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.value.chars().collect();
        chars.remove(self.cursor - 1);
        self.cursor -= 1;
        self.value = chars.into_iter().collect();
    }

    fn delete(&mut self) {
        let mut chars: Vec<char> = self.value.chars().collect();
        if self.cursor < chars.len() {
            chars.remove(self.cursor);
            self.value = chars.into_iter().collect();
        }
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }
}

#[derive(Clone)]
pub enum FileDialog {
    Rename {
        target: PathBuf,
        prompt: TextPrompt,
    },
    Create {
        parent: PathBuf,
        is_dir: bool,
        prompt: TextPrompt,
    },
    Delete {
        target: PathBuf,
        is_dir: bool,
        dirty: bool,
    },
    /// Add or edit a provider in pi's `models.json`.
    Provider {
        fields: Vec<TextPrompt>,
        selected: usize,
    },
}

/// Row order of the [`FileDialog::Provider`] form.
pub const PROVIDER_LABELS: [&str; 8] = [
    "provider id",
    "display name",
    "base url",
    "api key",
    "api",
    "model id",
    "model name",
    "context window",
];

/// An existing provider, opened in the form so it can be edited.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ProviderPrefill {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub api: String,
    pub model_id: String,
    pub model_name: String,
    pub context: String,
}

impl ProviderPrefill {
    fn from_entry(id: &str, entry: &Value) -> Self {
        let model = entry
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| models.first())
            .unwrap_or(&Value::Null);
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Self {
            id: id.to_string(),
            name: text("name"),
            base_url: text("baseUrl"),
            api_key: text("apiKey"),
            api: text("api"),
            model_id: model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            model_name: model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            context: model
                .get("contextWindow")
                .and_then(Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_default(),
        }
    }
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

#[derive(Clone)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub cwd: String,
    pub name: Option<String>,
    pub first_message: String,
    pub message_count: usize,
    pub modified: SystemTime,
}

impl SessionInfo {
    pub fn label(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let first = self.first_message.lines().next().unwrap_or_default().trim();
        if first.is_empty() {
            "(empty session)".to_string()
        } else {
            first.to_string()
        }
    }

    pub fn age(&self) -> String {
        let seconds = SystemTime::now()
            .duration_since(self.modified)
            .unwrap_or_default()
            .as_secs();
        match seconds {
            0..=59 => "now".to_string(),
            60..=3_599 => format!("{}m", seconds / 60),
            3_600..=86_399 => format!("{}h", seconds / 3_600),
            86_400..=604_799 => format!("{}d", seconds / 86_400),
            _ => format!("{}w", seconds / 604_800),
        }
    }
}

pub struct SessionHistory {
    pub sessions: Vec<SessionInfo>,
    pub selected: usize,
    pub confirm: Option<PathBuf>,
}

/// A model as pi reports it: provider plus id identify a model, name is what
/// the user sees, and the context window is only used for the picker label.
#[derive(Clone, Debug, Eq)]
pub struct ModelPref {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub context_window: Option<u64>,
}

impl PartialEq for ModelPref {
    fn eq(&self, other: &Self) -> bool {
        self.provider == other.provider && self.id == other.id
    }
}

impl ModelPref {
    pub fn from_value(value: &Value) -> Option<Self> {
        let provider = value.get("provider").and_then(Value::as_str)?.to_string();
        let id = value
            .get("id")
            .or_else(|| value.get("modelId"))
            .and_then(Value::as_str)?
            .to_string();
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| id.clone());
        let context_window = value.get("contextWindow").and_then(Value::as_u64);
        Some(Self {
            provider,
            id,
            name,
            context_window,
        })
    }

    pub fn label(&self) -> String {
        self.name.clone()
    }

    pub fn hint(&self) -> String {
        match self.context_window {
            Some(context) if context >= 1_000 => {
                format!("{} · {}k", self.provider, context / 1_000)
            }
            _ => self.provider.clone(),
        }
    }
}

pub struct PickerItem {
    pub label: String,
    pub hint: String,
    pub active: bool,
}

/// What Enter does with the highlighted row.
pub enum PickerAction {
    SetModel(Vec<ModelPref>),
    SetThinking(Vec<String>),
    EditProvider(Vec<ProviderPrefill>),
}

enum Choice {
    Model(ModelPref),
    Thinking(String),
    Provider(ProviderPrefill),
}

pub struct ListPicker {
    pub title: String,
    pub footer: String,
    pub items: Vec<PickerItem>,
    pub selected: usize,
    /// Set once the user moves the selection, so a late data refresh does not
    /// yank the cursor back to the active row.
    pub touched: bool,
    pub action: PickerAction,
}

pub struct Agent {
    pub entries: Vec<Entry>,
    pub input: String,
    pub input_cursor: usize,
    pub scroll: usize,
    pub busy: bool,
    pub suggestions: Vec<String>,
    pub suggestion_selected: usize,
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
            suggestions: Vec::new(),
            suggestion_selected: 0,
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

    fn push_tool(&mut self, id: &str, label: String, state: ToolState, body: Vec<ToolBodyLine>) {
        let index = self.entries.len();
        self.entries
            .push(Entry::Tool(ToolEntry { label, state, body }));
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
        self.clear_suggestions();
    }

    fn clear_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_selected = 0;
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

    fn reference_query(&self) -> Option<(usize, String)> {
        let chars: Vec<char> = self.input.chars().collect();
        let end = self.input_cursor.min(chars.len());
        let mut start = None;
        for index in (0..end).rev() {
            match chars[index] {
                '@' => {
                    if index == 0 || chars[index - 1].is_whitespace() {
                        start = Some(index);
                    }
                    break;
                }
                ch if ch.is_whitespace() => break,
                _ => {}
            }
        }
        let start = start?;
        Some((start, chars[start + 1..end].iter().collect()))
    }

    fn update_suggestions(&mut self, files: &[String]) {
        let Some((_, query)) = self.reference_query() else {
            self.clear_suggestions();
            return;
        };
        let query = query.to_lowercase();
        let mut matches: Vec<(u8, String)> = files
            .iter()
            .filter_map(|path| {
                let lower = path.to_lowercase();
                if !lower.contains(&query) {
                    return None;
                }
                let name = lower.rsplit('/').next().unwrap_or(&lower);
                let rank = if query.is_empty() || name.starts_with(&query) {
                    0
                } else if lower.starts_with(&query) {
                    1
                } else if name.contains(&query) {
                    2
                } else {
                    3
                };
                Some((rank, path.clone()))
            })
            .collect();
        matches.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        self.suggestions = matches.into_iter().take(8).map(|(_, path)| path).collect();
        if self.suggestion_selected >= self.suggestions.len() {
            self.suggestion_selected = self.suggestions.len().saturating_sub(1);
        }
    }

    fn move_suggestion(&mut self, delta: isize) {
        if self.suggestions.is_empty() {
            return;
        }
        let next = self.suggestion_selected as isize + delta;
        self.suggestion_selected =
            next.clamp(0, self.suggestions.len().saturating_sub(1) as isize) as usize;
    }

    fn accept_suggestion(&mut self) -> bool {
        if self.suggestions.is_empty() {
            return false;
        }
        let Some((start, _)) = self.reference_query() else {
            self.clear_suggestions();
            return false;
        };
        let index = self
            .suggestion_selected
            .min(self.suggestions.len().saturating_sub(1));
        let replacement = format!("@{} ", self.suggestions[index]);
        let mut chars: Vec<char> = self.input.chars().collect();
        let end = self.input_cursor.min(chars.len());
        chars.splice(start..end, replacement.chars());
        self.input = chars.into_iter().collect();
        self.input_cursor = start + replacement.chars().count();
        self.clear_suggestions();
        true
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
    pub project_files: Vec<String>,
    pub editor: Editor,
    pub agent: Agent,
    pub git: Git,
    pub pi: PiState,
    /// The pi binary to spawn, or `None` when pi is unavailable.
    pi_path: Option<PathBuf>,
    /// File passed to `--append-system-prompt`, written by smol at startup.
    instructions: Option<PathBuf>,
    pub focus: Focus,
    pub sidebar: Sidebar,
    pub should_quit: bool,
    pub status: Option<(String, Instant)>,
    pub tracks: Vec<Track>,
    pub ui: UiRects,
    pub now: Instant,
    pub started_at: Instant,
    last_tree_refresh: Instant,
    pub model: Option<String>,
    /// The model pi reports as active right now.
    pub model_current: Option<ModelPref>,
    /// Last model the user picked, remembered across launches.
    model_pref: Option<ModelPref>,
    model_pref_file: Option<PathBuf>,
    /// True while an automatic `set_model` (startup or new session) is in flight.
    model_auto_apply: bool,
    /// Persist the model reported by the next `get_state` (after a session switch).
    model_record_once: bool,
    pub picker: Option<ListPicker>,
    /// The thinking level pi reports, and the levels the model supports.
    pub thinking_level: Option<String>,
    pub thinking_levels: Vec<String>,
    /// `.smol/todo.md` list the model maintains, with per-task timing.
    pub todo: todo::TodoList,
    /// pi's `models.json`, where connections are saved.
    pub models_file: PathBuf,
    pub diff: Option<DiffView>,
    pub menu: Option<CommandMenu>,
    pub context_menu: Option<ContextMenu>,
    pub history: Option<SessionHistory>,
    pub session_path: Option<PathBuf>,
    pub session_dir: Option<PathBuf>,
    pub dialog: Option<FileDialog>,
    pub unsupported: Option<PathBuf>,
    pub dragging: bool,
    pub agent_hidden: bool,
    pub left_width: Option<u16>,
    pub right_width: Option<u16>,
    pub resizing: Option<ResizeEdge>,
    pub hover: Option<(u16, u16)>,
    pub hover_edge: Option<ResizeEdge>,
    pub follow_editor_cursor: bool,
    last_tree_click: Option<(Instant, Option<usize>)>,
    pointer: PointerShape,
    pointer_dirty: bool,
    pub commands: Vec<PiCommand>,
    pub models: Vec<Value>,
    context_percent: Option<u64>,
    stats_notice: bool,
    auto_compacted: bool,
    pending: HashMap<String, PendingTool>,
    pending_switch: Option<PathBuf>,
    pi_stderr: Vec<String>,
    pi_exited: bool,
    project_lines: Option<Receiver<Vec<String>>>,
}

impl App {
    pub fn new(root: PathBuf, force_no_pi: bool) -> Self {
        let tree = FileTree::new(&root);
        let project_files = tree.files();
        let editor = Editor::new(&root);
        let git = Git::new(&root);
        let project_lines = if cfg!(test) {
            None
        } else {
            let (tx, rx) = mpsc::channel();
            let scan_root = root.clone();
            thread::spawn(move || {
                let lines = crate::predict::scan_project(&scan_root);
                let _ = tx.send(lines);
            });
            Some(rx)
        };
        let model_pref_file = config_file("model.json");
        let model_pref = model_pref_file.as_deref().and_then(load_model_pref);
        let instructions = crate::prompt::write_instructions();
        let pi_path = if force_no_pi { None } else { find_pi() };

        let mut app = Self {
            root,
            tree,
            project_files,
            editor,
            agent: Agent::new(),
            git,
            pi: PiState::Missing,
            pi_path,
            instructions,
            focus: Focus::Tree,
            sidebar: Sidebar::Files,
            should_quit: false,
            status: None,
            tracks: Vec::new(),
            ui: UiRects::default(),
            now: Instant::now(),
            started_at: Instant::now(),
            last_tree_refresh: Instant::now(),
            model: None,
            model_current: None,
            model_pref,
            model_pref_file,
            model_auto_apply: false,
            model_record_once: false,
            picker: None,
            thinking_level: None,
            thinking_levels: Vec::new(),
            todo: todo::TodoList::new(),
            models_file: models_file(),
            diff: None,
            menu: None,
            context_menu: None,
            history: None,
            session_path: None,
            session_dir: None,
            dialog: None,
            unsupported: None,
            dragging: false,
            agent_hidden: false,
            left_width: None,
            right_width: None,
            resizing: None,
            hover: None,
            hover_edge: None,
            follow_editor_cursor: true,
            last_tree_click: None,
            pointer: PointerShape::Default,
            pointer_dirty: false,
            commands: Vec::new(),
            models: Vec::new(),
            context_percent: None,
            stats_notice: false,
            auto_compacted: false,
            pending: HashMap::new(),
            pending_switch: None,
            pi_stderr: Vec::new(),
            pi_exited: false,
            project_lines,
        };
        app.start_pi(None);
        app
    }

    /// Spawns pi with smol's flags: full permissions (`-a`), no extensions
    /// (`-ne`), no skills (`-ns`), the appended system prompt, and optionally
    /// a session to resume. Restarting this way keeps the conversation.
    fn start_pi(&mut self, resume: Option<PathBuf>) {
        if let PiState::Running(client) = &mut self.pi {
            client.shutdown();
        }
        let Some(path) = self.pi_path.clone() else {
            self.pi = PiState::Missing;
            return;
        };
        let mut args: Vec<String> = vec![
            "-a".to_string(),
            "-ne".to_string(),
            "-ns".to_string(),
        ];
        if let Some(instructions) = &self.instructions {
            args.push("--append-system-prompt".to_string());
            args.push(instructions.to_string_lossy().to_string());
        }
        if let Some(session) = resume {
            args.push("--session".to_string());
            args.push(session.to_string_lossy().to_string());
        }
        let spawned = PiClient::spawn_with(&path, &self.root, &args);
        self.pi = match spawned {
            Ok(client) => PiState::Running(client),
            Err(error) => PiState::Failed(format!("could not start pi: {error}")),
        };
        self.init_pi();
    }

    /// The handshake every fresh pi process needs, including re-applying the
    /// remembered model.
    fn init_pi(&mut self) {
        if !matches!(self.pi, PiState::Running(_)) {
            return;
        }
        self.send_rpc(json!({ "type": "get_state" }));
        self.send_rpc(json!({ "type": "get_commands" }));
        self.send_rpc(json!({ "type": "get_available_models" }));
        self.send_rpc(json!({ "type": "set_auto_compaction", "enabled": false }));
        self.send_rpc(json!({ "type": "get_session_stats" }));
        self.apply_model_pref();
    }

    /// Restarts pi, resuming the open session so the transcript survives.
    pub fn restart_pi(&mut self) {
        let resume = self.session_path.clone();
        self.start_pi(resume);
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
                self.refresh_todo();
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
            if command == "set_model" && std::mem::take(&mut self.model_auto_apply) {
                // The remembered model is gone from pi's config: drop it so smol
                // stops asking for it on every launch.
                self.forget_model_pref();
                self.agent
                    .note("saved model is unavailable, using pi's default");
                return;
            }
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            if command == "switch_session" {
                self.pending_switch = None;
            }
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
                    self.model_current = ModelPref::from_value(model);
                    if std::mem::take(&mut self.model_record_once) {
                        if let Some(pref) = self.model_current.clone() {
                            self.remember_model_pref(pref);
                        }
                    }
                }
                if let Some(streaming) = value.pointer("/data/isStreaming").and_then(Value::as_bool)
                {
                    self.agent.busy = streaming;
                }
                if let Some(path) = value.pointer("/data/sessionFile").and_then(Value::as_str) {
                    self.remember_session(path);
                }
                if let Some(level) = value.pointer("/data/thinkingLevel").and_then(Value::as_str) {
                    self.thinking_level = Some(level.to_string());
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
                self.refresh_picker();
            }
            "get_available_thinking_levels" => {
                self.thinking_levels = value
                    .pointer("/data/levels")
                    .and_then(Value::as_array)
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                self.refresh_picker();
            }
            "set_model" => {
                self.model_auto_apply = false;
                let data = value.get("data");
                let pref = data.and_then(|data| {
                    ModelPref::from_value(data)
                        .or_else(|| data.pointer("/model").and_then(ModelPref::from_value))
                });
                if let Some(pref) = pref {
                    self.model = Some(pref.label());
                    self.remember_model_pref(pref.clone());
                    self.set_status(format!("model {}", pref.label()));
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
                self.send_rpc(json!({ "type": "get_state" }));
                self.apply_model_pref();
            }
            "switch_session" => {
                let cancelled = value
                    .pointer("/data/cancelled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let path = self.pending_switch.take();
                if cancelled {
                    self.set_status("session switch cancelled");
                } else if let Some(path) = path {
                    self.remember_session_path(&path);
                    self.agent.reset();
                    self.context_percent = None;
                    self.auto_compacted = false;
                    // The resumed session brings its own model; remember it as
                    // the last used one instead of overriding it.
                    self.model_record_once = true;
                    self.set_status("loading session");
                    self.send_rpc(json!({ "type": "get_messages" }));
                    self.send_rpc(json!({ "type": "get_state" }));
                    self.send_rpc(json!({ "type": "get_session_stats" }));
                }
            }
            "get_messages" => {
                if let Some(messages) = value.pointer("/data/messages").and_then(Value::as_array) {
                    self.load_transcript(messages);
                }
            }
            "set_thinking_level" => {
                if let Some(level) = value.pointer("/data/level").and_then(Value::as_str) {
                    self.thinking_level = Some(level.to_string());
                }
                let level = self.thinking_level.clone().unwrap_or_default();
                if !level.is_empty() {
                    self.agent.note(format!("thinking {level}"));
                }
                // The level is clamped to the model, so ask pi what stuck.
                self.send_rpc(json!({ "type": "get_state" }));
            }
            "get_session_stats" => {
                if let Some(path) = value.pointer("/data/sessionFile").and_then(Value::as_str) {
                    self.remember_session(path);
                }
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
        let body = tool_body(name, &args);
        self.agent.push_tool(&id, label, ToolState::Running, body);

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
        self.refresh_todo();

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
                self.refresh_tree();
                if !failed {
                    self.follow_file(&path);
                    self.reload_after_tool(&path);
                }
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

        let todo_due = self
            .todo
            .last_poll
            .map(|at| self.now.duration_since(at) >= Duration::from_millis(400))
            .unwrap_or(true);
        if todo_due {
            self.refresh_todo();
        }

        if self.last_tree_refresh.elapsed() >= Duration::from_secs(2) {
            self.refresh_tree();
        }

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

        let mut project_lines = None;
        let mut project_lines_done = false;
        if let Some(rx) = &self.project_lines {
            match rx.try_recv() {
                Ok(lines) => project_lines = Some(lines),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => project_lines_done = true,
            }
        }
        if project_lines_done {
            self.project_lines = None;
        }
        if let Some(lines) = project_lines {
            self.project_lines = None;
            self.editor.add_project_lines(lines);
        }

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

    /// Reads `<project>/.smol/todo.md` into the panel state. The file is the
    /// model's; smol only parses and times it.
    fn refresh_todo(&mut self) {
        let path = self.root.join(".smol").join("todo.md");
        self.todo.last_poll = Some(self.now);
        match fs::read_to_string(&path) {
            Ok(text) => self.todo.apply_read(todo::parse(&text), self.now),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !self.todo.is_empty() {
                    self.todo.apply(Vec::new(), self.now);
                }
            }
            Err(_) => {}
        }
    }

    fn refresh_tree(&mut self) {
        self.tree.refresh();
        self.project_files = self.tree.files();
        let removed = self.editor.close_missing();
        if !removed.is_empty() {
            self.set_status(if removed.len() == 1 {
                format!("closed deleted {}", removed[0])
            } else {
                format!("closed {} deleted files", removed.len())
            });
        }
        if !self.agent.suggestions.is_empty() {
            self.agent.update_suggestions(&self.project_files);
        }
        self.last_tree_refresh = Instant::now();
    }

    fn refresh_agent_suggestions(&mut self) {
        self.agent.suggestion_selected = 0;
        self.agent.update_suggestions(&self.project_files);
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

    fn remember_session(&mut self, path: &str) {
        self.remember_session_path(&PathBuf::from(path));
    }

    fn remember_session_path(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.session_dir = Some(parent.to_path_buf());
        }
        self.session_path = Some(path.to_path_buf());
    }

    fn sessions_dir(&self) -> PathBuf {
        if let Some(dir) = &self.session_dir {
            return dir.clone();
        }
        if let Some(path) = &self.session_path {
            if let Some(parent) = path.parent() {
                return parent.to_path_buf();
            }
        }
        if let Some(dir) = env::var_os("PI_CODING_AGENT_SESSION_DIR") {
            return PathBuf::from(dir);
        }
        let agent_dir = env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .or_else(|| env::var_os("USERPROFILE"))
                    .map(|home| PathBuf::from(home).join(".pi").join("agent"))
            })
            .unwrap_or_else(|| PathBuf::from(".pi").join("agent"));
        let cwd = self.root.to_string_lossy();
        let safe = cwd
            .trim_start_matches(['/', '\\'])
            .replace(['/', '\\', ':'], "-");
        agent_dir.join("sessions").join(format!("--{safe}--"))
    }

    fn open_history(&mut self) {
        if self.agent.busy {
            self.set_status("stop the agent before opening history");
            return;
        }
        let sessions = list_sessions(&self.sessions_dir(), &self.root);
        let selected = sessions
            .iter()
            .position(|session| self.session_path.as_ref() == Some(&session.path))
            .unwrap_or(0);
        self.menu = None;
        self.context_menu = None;
        self.dialog = None;
        self.history = Some(SessionHistory {
            sessions,
            selected,
            confirm: None,
        });
    }

    fn refresh_history(&mut self) {
        let Some(history) = self.history.as_ref() else {
            return;
        };
        let selected = history.selected;
        let confirm = history.confirm.clone();
        let sessions = list_sessions(&self.sessions_dir(), &self.root);
        if let Some(history) = self.history.as_mut() {
            history.sessions = sessions;
            history.selected = selected.min(history.sessions.len().saturating_sub(1));
            if confirm.is_some_and(|path| {
                !history.sessions.iter().any(|session| session.path == path)
            }) {
                history.confirm = None;
            }
        }
    }

    fn resume_session(&mut self, path: PathBuf) {
        if self.agent.busy {
            self.set_status("stop the agent before switching sessions");
            return;
        }
        if !self.ensure_pi() {
            return;
        }
        self.pending_switch = Some(path.clone());
        self.history = None;
        self.set_status("loading session");
        self.send_rpc(json!({
            "type": "switch_session",
            "sessionPath": path.to_string_lossy().to_string()
        }));
    }

    fn delete_session(&mut self, path: PathBuf) {
        let active = self.session_path.as_deref() == Some(path.as_path());
        if active {
            self.send_rpc(json!({ "type": "new_session" }));
            self.agent.reset();
            self.context_percent = None;
            self.auto_compacted = false;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                if let Some(history) = self.history.as_mut() {
                    history.sessions.retain(|session| session.path != path);
                    history.selected =
                        history.selected.min(history.sessions.len().saturating_sub(1));
                    history.confirm = None;
                }
                self.set_status("deleted session");
            }
            Err(error) => {
                if let Some(history) = self.history.as_mut() {
                    history.confirm = None;
                }
                self.set_status(format!("delete failed: {error}"));
            }
        }
    }

    fn handle_history_key(&mut self, key: KeyEvent) {
        let mut close = false;
        let mut refresh = false;
        let mut resume = None;
        let mut delete = None;
        let mut confirm = false;
        if let Some(history) = self.history.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    if history.confirm.take().is_none() {
                        close = true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    history.selected = history.selected.saturating_sub(1);
                    history.confirm = None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    history.selected = (history.selected + 1)
                        .min(history.sessions.len().saturating_sub(1));
                    history.confirm = None;
                }
                KeyCode::Home => {
                    history.selected = 0;
                    history.confirm = None;
                }
                KeyCode::End => {
                    history.selected = history.sessions.len().saturating_sub(1);
                    history.confirm = None;
                }
                KeyCode::PageUp => {
                    history.selected = history.selected.saturating_sub(8);
                    history.confirm = None;
                }
                KeyCode::PageDown => {
                    history.selected = (history.selected + 8)
                        .min(history.sessions.len().saturating_sub(1));
                    history.confirm = None;
                }
                KeyCode::Char('r') => refresh = true,
                KeyCode::Char('d') | KeyCode::Delete => {
                    if let Some(session) = history.sessions.get(history.selected) {
                        let path = session.path.clone();
                        if history.confirm.as_deref() == Some(path.as_path()) {
                            delete = Some(path);
                        } else {
                            history.confirm = Some(path);
                            confirm = true;
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(path) = history.confirm.clone() {
                        delete = Some(path);
                    } else if let Some(session) = history.sessions.get(history.selected) {
                        resume = Some(session.path.clone());
                    }
                }
                _ => {}
            }
        }
        if close {
            self.history = None;
        } else if refresh {
            self.refresh_history();
        } else if let Some(path) = delete {
            self.delete_session(path);
        } else if let Some(path) = resume {
            self.resume_session(path);
        } else if confirm {
            self.set_status("press d or enter to delete");
        }
    }

    fn load_transcript(&mut self, messages: &[Value]) {
        self.agent.reset();
        for message in messages {
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match role {
                "user" => {
                    let text = message_text(message);
                    if !text.trim().is_empty() {
                        self.agent.entries.push(Entry::User(text));
                    }
                }
                "assistant" => {
                    let mut text = String::new();
                    let mut thinking = String::new();
                    let mut calls = Vec::new();
                    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    if let Some(part) =
                                        block.get("text").and_then(Value::as_str)
                                    {
                                        if !text.is_empty() {
                                            text.push('\n');
                                        }
                                        text.push_str(part);
                                    }
                                }
                                Some("thinking") => {
                                    if let Some(part) =
                                        block.get("thinking").and_then(Value::as_str)
                                    {
                                        if !thinking.is_empty() {
                                            thinking.push('\n');
                                        }
                                        thinking.push_str(part);
                                    }
                                }
                                Some("toolCall") => {
                                    let id = block
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    let name = block
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("tool")
                                        .to_string();
                                    let args =
                                        block.get("arguments").cloned().unwrap_or(Value::Null);
                                    calls.push((id, name, args));
                                }
                                _ => {}
                            }
                        }
                    }
                    if !text.trim().is_empty() || !thinking.trim().is_empty() {
                        self.agent.entries.push(Entry::Assistant {
                            text,
                            thinking,
                            thinking_open: false,
                        });
                    }
                    for (id, name, args) in calls {
                        let path = tool_path(&args).map(|raw| self.resolve(&raw));
                        let label = tool_label(&name, &args, path.as_deref(), &self.root);
                        let body = tool_body(&name, &args);
                        self.agent.push_tool(&id, label, ToolState::Running, body);
                    }
                }
                "toolResult" => {
                    let id = message
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let failed = message
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.agent.update_tool(
                        id,
                        if failed {
                            ToolState::Failed
                        } else {
                            ToolState::Done
                        },
                    );
                }
                _ => {}
            }
        }
        for entry in &mut self.agent.entries {
            if let Entry::Tool(tool) = entry {
                if tool.state == ToolState::Running {
                    tool.state = ToolState::Done;
                }
            }
        }
        self.agent.scroll = 0;
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
            "history" | "resume" => self.open_history(),
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
                Some(text) => {
                    self.editor.clipboard = text.clone();
                    match copy_to_clipboard(&text) {
                        Ok(()) => self.set_status("copied"),
                        Err(error) => self.agent.note(error),
                    }
                }
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
            self.open_model_picker();
            return;
        }
        // An explicit choice replaces any pending automatic apply.
        self.model_auto_apply = false;
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
                .note("use /model provider/model, or /model to pick from a list");
        }
    }

    /// Opens the selectable model list and asks pi for a fresh model list to
    /// fill it with.
    pub fn open_model_picker(&mut self) {
        if matches!(self.pi, PiState::Running(_)) {
            self.send_rpc(json!({ "type": "get_available_models" }));
        } else if self.models.is_empty() {
            match &self.pi {
                PiState::Failed(error) => self.set_status(error.clone()),
                _ => self.set_status("pi is not installed"),
            }
            return;
        }
        self.close_overlays();
        self.picker = Some(self.build_model_picker());
    }

    fn build_model_picker(&self) -> ListPicker {
        let models = model_list(&self.models);
        let items = models
            .iter()
            .map(|model| PickerItem {
                label: model.label(),
                hint: model.hint(),
                active: self.model_current.as_ref() == Some(model),
            })
            .collect();
        let selected = model_position(&models, self.model_current.as_ref());
        ListPicker {
            title: format!("model · {}", models.len()),
            footer: "up/down select  enter switch  esc close".to_string(),
            items,
            selected,
            touched: false,
            action: PickerAction::SetModel(models),
        }
    }

    /// Opens the thinking-level list, asking pi which levels the current
    /// model actually supports.
    pub fn open_thinking_picker(&mut self) {
        if matches!(self.pi, PiState::Running(_)) {
            self.send_rpc(json!({ "type": "get_available_thinking_levels" }));
        }
        self.close_overlays();
        self.picker = Some(self.build_thinking_picker());
    }

    fn build_thinking_picker(&self) -> ListPicker {
        let levels: Vec<String> = if self.thinking_levels.is_empty() {
            THINKING_LEVELS
                .iter()
                .map(|level| level.to_string())
                .collect()
        } else {
            self.thinking_levels.clone()
        };
        let items = levels
            .iter()
            .map(|level| PickerItem {
                label: level.clone(),
                hint: String::new(),
                active: self.thinking_level.as_deref() == Some(level.as_str()),
            })
            .collect();
        let selected = self
            .thinking_level
            .as_deref()
            .and_then(|current| levels.iter().position(|level| level == current))
            .unwrap_or(0);
        ListPicker {
            title: format!("thinking · {}", levels.len()),
            footer: "up/down select  enter set  esc close".to_string(),
            items,
            selected,
            touched: false,
            action: PickerAction::SetThinking(levels),
        }
    }

    /// Opens the list of configured providers; the first row creates a new
    /// connection, the rest open that provider for editing.
    pub fn open_provider_picker(&mut self) {
        if let Err(error) = read_providers(&self.models_file) {
            self.set_status(error);
            return;
        }
        self.close_overlays();
        self.picker = Some(self.build_provider_picker());
    }

    fn build_provider_picker(&self) -> ListPicker {
        let mut providers = read_providers(&self.models_file).unwrap_or_default();
        let mut items = vec![PickerItem {
            label: "new provider".to_string(),
            hint: "connect".to_string(),
            active: false,
        }];
        let current = self.model_current.as_ref().map(|model| model.provider.clone());
        let mut selected = 0;
        for (index, provider) in providers.iter().enumerate() {
            let active = current.as_deref() == Some(provider.id.as_str());
            if active {
                selected = index + 1;
            }
            items.push(PickerItem {
                label: if provider.name.is_empty() {
                    provider.id.clone()
                } else {
                    provider.name.clone()
                },
                hint: provider.base_url.clone(),
                active,
            });
        }
        providers.insert(
            0,
            ProviderPrefill {
                api: "openai-completions".to_string(),
                context: "131072".to_string(),
                ..ProviderPrefill::default()
            },
        );
        ListPicker {
            title: format!("provider · {}", items.len() - 1),
            footer: "up/down select  enter connect  esc close".to_string(),
            items,
            selected,
            touched: false,
            action: PickerAction::EditProvider(providers),
        }
    }

    /// Opens the connect form, prefilled from the chosen provider (or blank
    /// for a new one).
    pub fn open_provider_form(&mut self, prefill: ProviderPrefill) {
        let values = [
            prefill.id,
            prefill.name,
            prefill.base_url,
            prefill.api_key,
            if prefill.api.is_empty() {
                "openai-completions".to_string()
            } else {
                prefill.api
            },
            prefill.model_id,
            prefill.model_name,
            if prefill.context.is_empty() {
                "131072".to_string()
            } else {
                prefill.context
            },
        ];
        let fields = PROVIDER_LABELS
            .iter()
            .zip(values)
            .map(|(label, value)| TextPrompt::new(*label, value))
            .collect();
        self.picker = None;
        self.close_overlays();
        self.dialog = Some(FileDialog::Provider {
            fields,
            selected: 0,
        });
    }

    fn close_overlays(&mut self) {
        self.menu = None;
        self.context_menu = None;
        self.dialog = None;
        self.history = None;
    }

    /// Re-reads the picker's source data without losing the row the user is
    /// on.
    fn refresh_picker(&mut self) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let is_model = matches!(&picker.action, PickerAction::SetModel(_));
        let touched = picker.touched;
        let previous = picker.items.get(picker.selected).map(|item| item.label.clone());
        let fallback = picker.selected;

        let mut fresh = if is_model {
            self.build_model_picker()
        } else if matches!(&picker.action, PickerAction::SetThinking(_)) {
            self.build_thinking_picker()
        } else {
            self.build_provider_picker()
        };
        if touched {
            let position = previous
                .and_then(|previous| fresh.items.iter().position(|item| item.label == previous));
            fresh.selected =
                position.unwrap_or_else(|| fallback.min(fresh.items.len().saturating_sub(1)));
            fresh.touched = true;
        }
        self.picker = Some(fresh);
    }

    fn handle_picker_key(&mut self, key: KeyEvent) {
        let mut apply = false;
        let mut close = false;
        if let Some(picker) = self.picker.as_mut() {
            let last = picker.items.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Up | KeyCode::Char('k') => {
                    picker.selected = picker.selected.saturating_sub(1);
                    picker.touched = true;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    picker.selected = (picker.selected + 1).min(last);
                    picker.touched = true;
                }
                KeyCode::PageUp => {
                    picker.selected = picker.selected.saturating_sub(8);
                    picker.touched = true;
                }
                KeyCode::PageDown => {
                    picker.selected = (picker.selected + 8).min(last);
                    picker.touched = true;
                }
                KeyCode::Home => {
                    picker.selected = 0;
                    picker.touched = true;
                }
                KeyCode::End => {
                    picker.selected = last;
                    picker.touched = true;
                }
                KeyCode::Enter => apply = true,
                _ => {}
            }
        }
        if close {
            self.picker = None;
            return;
        }
        if apply {
            self.apply_selected_picker_row();
        }
    }

    /// Runs the highlighted row: switch model or set the thinking level. The
    /// picker stays open when there is nothing to apply yet.
    fn apply_selected_picker_row(&mut self) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let selected = picker.selected;
        let choice = match &picker.action {
            PickerAction::SetModel(models) => models.get(selected).cloned().map(Choice::Model),
            PickerAction::SetThinking(levels) => {
                levels.get(selected).cloned().map(Choice::Thinking)
            }
            PickerAction::EditProvider(providers) => {
                providers.get(selected).cloned().map(Choice::Provider)
            }
        };
        match choice {
            Some(Choice::Model(pref)) => {
                self.model_auto_apply = false;
                self.send_rpc(json!({
                    "type": "set_model",
                    "provider": pref.provider,
                    "modelId": pref.id
                }));
                self.picker = None;
            }
            Some(Choice::Thinking(level)) => {
                self.thinking_level = Some(level.clone());
                self.send_rpc(json!({ "type": "set_thinking_level", "level": level }));
                self.picker = None;
            }
            Some(Choice::Provider(prefill)) => self.open_provider_form(prefill),
            None => self.picker = Some(picker),
        }
    }

    fn apply_model_pref(&mut self) {
        let Some(pref) = self.model_pref.clone() else {
            return;
        };
        if !matches!(self.pi, PiState::Running(_)) {
            return;
        }
        self.model_auto_apply = true;
        self.send_rpc(json!({
            "type": "set_model",
            "provider": pref.provider,
            "modelId": pref.id
        }));
    }

    /// Stores the model as the one to restore on the next launch.
    fn remember_model_pref(&mut self, pref: ModelPref) {
        self.model_current = Some(pref.clone());
        self.model_pref = Some(pref.clone());
        if let Some(path) = self.model_pref_file.clone() {
            if let Err(error) = save_model_pref(&path, &pref) {
                self.set_status(format!("could not remember model: {error}"));
            }
        }
    }

    fn forget_model_pref(&mut self) {
        self.model_pref = None;
        if let Some(path) = self.model_pref_file.clone() {
            let _ = fs::remove_file(path);
        }
    }

    fn thinking_command(&mut self, args: &str) {
        if args.is_empty() {
            self.open_thinking_picker();
            return;
        }
        if THINKING_LEVELS.contains(&args) {
            self.thinking_level = Some(args.to_string());
            self.send_rpc(json!({ "type": "set_thinking_level", "level": args }));
        } else {
            self.agent
                .note(format!("thinking levels  {}", THINKING_LEVELS.join("  ")));
        }
    }

    fn command_help(&self) -> String {
        let mut text = String::from(
            "commands\n/help  /new  /history  /compact [instructions]  /model  /thinking [level]\n/abort  /stats  /name [name]  /export [path]  /copy  /hide  /show\n",
        );
        text.push_str("/model and /thinking open lists to pick from\n");
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
            "\n/settings /tree /fork /clone /share /login run inside pi's terminal",
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
        if self.dialog.is_some() {
            self.handle_dialog_key(key, ctrl, alt);
            return;
        }
        if self.history.is_some() {
            self.handle_history_key(key);
            return;
        }
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return;
        }
        if self.context_menu.is_some() {
            self.handle_context_key(key);
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
        if ctrl && key.code == KeyCode::Char('r') {
            self.open_history();
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
                        self.copy_text(&text, "copied");
                    }
                }
            }
            return;
        }
        if ctrl && key.code == KeyCode::Char('x') {
            // The diff covers the editor, so cutting would edit blind.
            if self.focus == Focus::Editor && self.diff.is_none() {
                if let Some(text) = self.editor.cut_selection() {
                    self.copy_text(&text, "cut");
                }
            }
            return;
        }
        if ctrl && key.code == KeyCode::Char('v') {
            match self.focus {
                Focus::Editor if self.diff.is_none() => self.paste_editor(),
                Focus::Agent => self.paste_agent(),
                _ => {}
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

        if self.diff.is_some() && self.handle_diff_key(key) {
            return;
        }

        match self.focus {
            Focus::Tree => self.handle_sidebar_key(key, ctrl, alt),
            Focus::Editor => self.handle_editor_key(key, ctrl, alt),
            Focus::Agent => self.handle_agent_key(key, ctrl, alt),
        }
    }

    /// Returns true when the open diff view consumed the key.
    ///
    /// The diff only takes over the keys of the pane it replaces, the editor.
    /// The sidebar, a focused commit input, and the agent prompt keep their
    /// keys, so a diff can never block browsing changes, committing, or typing
    /// a prompt.
    fn handle_diff_key(&mut self, key: KeyEvent) -> bool {
        let prompt_owns_keys = match self.focus {
            Focus::Editor => false,
            Focus::Tree => self.git.input_focus,
            Focus::Agent => true,
        };

        match key.code {
            // Esc closes the diff, except while the agent needs it to stop a
            // run or to close the file suggestions.
            KeyCode::Esc => {
                if self.focus == Focus::Agent
                    && (self.agent.busy || !self.agent.suggestions.is_empty())
                {
                    return false;
                }
                self.diff = None;
                true
            }
            KeyCode::Char('q') if !prompt_owns_keys => {
                self.diff = None;
                true
            }
            // The editor is covered by the diff, so its keys would edit blind.
            _ if self.focus == Focus::Editor => {
                if let Some(diff) = &mut self.diff {
                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            diff.scroll = diff.scroll.saturating_sub(1)
                        }
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
                true
            }
            _ => false,
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
            Ok(message) => {
                self.diff = None;
                self.set_status(message);
            }
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
                submenu_item("Settings", 7, MenuPageKind::Settings),
                submenu_item("LLM commands", 6, MenuPageKind::Llm),
                submenu_item("Git", 2, MenuPageKind::Git),
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
                        MenuItem {
                            label: "Session history".to_string(),
                            hint: "ctrl+r".to_string(),
                            action: MenuAction::SessionHistory,
                        },
                        MenuItem {
                            label: "Model".to_string(),
                            hint: self
                                .model
                                .clone()
                                .unwrap_or_else(|| "select".to_string()),
                            action: MenuAction::ModelPicker,
                        },
                        MenuItem {
                            label: "Thinking".to_string(),
                            hint: self
                                .thinking_level
                                .clone()
                                .unwrap_or_else(|| "select".to_string()),
                            action: MenuAction::Thinking,
                        },
                        MenuItem {
                            label: "Provider".to_string(),
                            hint: "connect".to_string(),
                            action: MenuAction::Provider,
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
                items: vec![
                    MenuItem {
                        label: "Refresh git status".to_string(),
                        hint: "r".to_string(),
                        action: MenuAction::RefreshGit,
                    },
                    MenuItem {
                        label: "Push".to_string(),
                        hint: "p".to_string(),
                        action: MenuAction::PushGit,
                    },
                ],
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
            MenuAction::PushGit => self.push_git(),
            MenuAction::ExplainFile => self.explain_file_with_pi(),
            MenuAction::ReviewFile => self.review_file_with_pi(),
            MenuAction::ExplainSelection => self.explain_selection_with_pi(),
            MenuAction::RefactorSelection => self.refactor_selection_with_pi(),
            MenuAction::AddTests => self.add_tests_with_pi(),
            MenuAction::FormatFile => self.format_file_with_pi(),
            MenuAction::SessionHistory => self.open_history(),
            MenuAction::ModelPicker => self.open_model_picker(),
            MenuAction::Thinking => self.open_thinking_picker(),
            MenuAction::Provider => self.open_provider_picker(),
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

    fn context_items(is_dir: bool, expanded: bool) -> Vec<ContextMenuItem> {
        let first = if is_dir {
            if expanded {
                "Collapse folder"
            } else {
                "Expand folder"
            }
        } else {
            "Open"
        };
        vec![
            ContextMenuItem {
                label: first,
                hint: "enter",
                action: ContextAction::Open,
            },
            ContextMenuItem {
                label: "Open in default app",
                hint: "",
                action: ContextAction::OpenDefault,
            },
            ContextMenuItem {
                label: "New file",
                hint: "",
                action: ContextAction::NewFile,
            },
            ContextMenuItem {
                label: "New folder",
                hint: "",
                action: ContextAction::NewFolder,
            },
            ContextMenuItem {
                label: "Rename",
                hint: "",
                action: ContextAction::Rename,
            },
            ContextMenuItem {
                label: "Delete",
                hint: "",
                action: ContextAction::Delete,
            },
            ContextMenuItem {
                label: "Copy path",
                hint: "",
                action: ContextAction::CopyPath,
            },
            ContextMenuItem {
                label: "Copy relative path",
                hint: "",
                action: ContextAction::CopyRelativePath,
            },
        ]
    }

    fn open_context_menu(&mut self, x: u16, y: u16) {
        let Some(node) = self.tree.selected_node() else {
            return;
        };
        let target = node.path.clone();
        let is_dir = node.is_dir;
        let expanded = node.expanded;
        self.context_menu = Some(ContextMenu {
            target,
            is_dir,
            selected: 0,
            anchor: (x, y),
            items: Self::context_items(is_dir, expanded),
        });
    }

    fn handle_context_key(&mut self, key: KeyEvent) {
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.context_menu = None,
            KeyCode::Up | KeyCode::Char('k') => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1));
            }
            KeyCode::Home => menu.selected = 0,
            KeyCode::End => menu.selected = menu.items.len().saturating_sub(1),
            KeyCode::Enter => {
                if let Some(action) = menu.items.get(menu.selected).map(|item| item.action) {
                    self.run_context_action(action);
                }
            }
            _ => {}
        }
    }

    fn open_create_dialog(&mut self, parent: PathBuf, is_dir: bool) {
        let kind = if is_dir { "folder" } else { "file" };
        let location = self.editor.rel_of(&parent);
        self.dialog = Some(FileDialog::Create {
            parent,
            is_dir,
            prompt: TextPrompt::new(format!("new {kind} in {location}"), ""),
        });
    }

    fn run_context_action(&mut self, action: ContextAction) {
        let Some(menu) = self.context_menu.take() else {
            return;
        };
        let target = menu.target;
        let is_dir = menu.is_dir;
        match action {
            ContextAction::Open => {
                if is_dir {
                    self.tree.toggle_selected();
                } else {
                    self.open_file(&target);
                }
            }
            ContextAction::OpenDefault => match open_in_default_app(&target) {
                Ok(()) => self.set_status("opened in default app"),
                Err(error) => self.set_status(error),
            },
            ContextAction::NewFile | ContextAction::NewFolder => {
                let parent = if is_dir {
                    target.clone()
                } else {
                    target
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| self.root.clone())
                };
                self.open_create_dialog(parent, matches!(action, ContextAction::NewFolder));
            }
            ContextAction::Rename => {
                let name = target
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.dialog = Some(FileDialog::Rename {
                    target,
                    prompt: TextPrompt::new(format!("rename {name}"), name),
                });
            }
            ContextAction::Delete => {
                let dirty = self.editor.buffers.iter().any(|buffer| {
                    buffer.dirty && (buffer.path == target || buffer.path.starts_with(&target))
                });
                self.dialog = Some(FileDialog::Delete {
                    target,
                    is_dir,
                    dirty,
                });
            }
            ContextAction::CopyPath => {
                let text = target.display().to_string();
                self.copy_text(&text, "copied path");
            }
            ContextAction::CopyRelativePath => {
                let text = self.editor.rel_of(&target).replace('\\', "/");
                self.copy_text(&text, "copied relative path");
            }
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool) {
        let mut submit = false;
        let mut cancel = false;
        let paste = (ctrl && key.code == KeyCode::Char('v'))
            .then(|| self.clipboard_text())
            .flatten();
        if let Some(dialog) = self.dialog.as_mut() {
            match dialog {
                FileDialog::Delete { .. } => match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => cancel = true,
                    KeyCode::Enter | KeyCode::Char('y') => submit = true,
                    _ => {}
                },
                FileDialog::Provider { fields, selected } => {
                    let last = fields.len().saturating_sub(1);
                    match key.code {
                        KeyCode::Esc => cancel = true,
                        KeyCode::Enter => submit = true,
                        KeyCode::Up | KeyCode::BackTab => {
                            *selected = selected.saturating_sub(1)
                        }
                        KeyCode::Down | KeyCode::Tab => {
                            *selected = (*selected + 1).min(last)
                        }
                        _ => {
                            if let Some(field) = fields.get_mut(*selected) {
                                match key.code {
                                    KeyCode::Backspace => field.backspace(),
                                    KeyCode::Delete => field.delete(),
                                    KeyCode::Left => field.left(),
                                    KeyCode::Right => field.right(),
                                    KeyCode::Home => field.cursor = 0,
                                    KeyCode::End => {
                                        field.cursor = field.value.chars().count()
                                    }
                                    KeyCode::Char('v') if ctrl => {
                                        if let Some(text) = paste.as_deref() {
                                            field.insert_text(text);
                                        }
                                    }
                                    KeyCode::Char('u') if ctrl => {
                                        field.value.clear();
                                        field.cursor = 0;
                                    }
                                    KeyCode::Char(ch) if !ctrl && !alt => field.insert(ch),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                FileDialog::Rename { prompt, .. } | FileDialog::Create { prompt, .. } => {
                    match key.code {
                        KeyCode::Esc => cancel = true,
                        KeyCode::Enter => submit = true,
                        KeyCode::Backspace => prompt.backspace(),
                        KeyCode::Delete => prompt.delete(),
                        KeyCode::Left => prompt.left(),
                        KeyCode::Right => prompt.right(),
                        KeyCode::Home => prompt.cursor = 0,
                        KeyCode::End => prompt.cursor = prompt.value.chars().count(),
                        KeyCode::Char('v') if ctrl => {
                            if let Some(text) = paste.as_deref() {
                                prompt.insert_text(text);
                            }
                        }
                        KeyCode::Char('u') if ctrl => {
                            prompt.value.clear();
                            prompt.cursor = 0;
                        }
                        KeyCode::Char(ch) if !ctrl && !alt => prompt.insert(ch),
                        _ => {}
                    }
                }
            }
        }
        if cancel {
            self.dialog = None;
        } else if submit {
            self.submit_dialog();
        }
    }

    fn submit_dialog(&mut self) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        match dialog {
            FileDialog::Rename { target, prompt } => {
                let parent = target
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.root.clone());
                let path = match child_path(&parent, &prompt.value) {
                    Ok(path) => path,
                    Err(error) => {
                        self.set_status(error);
                        return;
                    }
                };
                if path == target {
                    self.dialog = None;
                    self.set_status("name unchanged");
                    return;
                }
                if fs::symlink_metadata(&path).is_ok() {
                    self.set_status(format!("{} already exists", self.editor.rel_of(&path)));
                    return;
                }
                if let Err(error) = fs::rename(&target, &path) {
                    self.set_status(format!("rename failed: {error}"));
                    return;
                }
                self.dialog = None;
                let path = fs::canonicalize(&path).unwrap_or(path);
                self.editor.rename_path(&target, &path);
                if let Some(open) = self.unsupported.take() {
                    self.unsupported = Some(remap_path(&open, &target, &path));
                }
                for track in &mut self.tracks {
                    track.path = remap_path(&track.path, &target, &path);
                }
                self.refresh_tree();
                self.tree.reveal(&path);
                self.set_status(format!("renamed to {}", self.editor.rel_of(&path)));
            }
            FileDialog::Create {
                parent,
                is_dir,
                prompt,
            } => {
                let path = match child_path(&parent, &prompt.value) {
                    Ok(path) => path,
                    Err(error) => {
                        self.set_status(error);
                        return;
                    }
                };
                if fs::symlink_metadata(&path).is_ok() {
                    self.set_status(format!("{} already exists", self.editor.rel_of(&path)));
                    return;
                }
                let result = if is_dir {
                    fs::create_dir(&path)
                } else {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map(drop)
                };
                if let Err(error) = result {
                    self.set_status(format!("create failed: {error}"));
                    return;
                }
                self.dialog = None;
                self.refresh_tree();
                if is_dir {
                    self.tree.reveal(&path);
                    self.focus = Focus::Tree;
                    self.set_status(format!("created {}", self.editor.rel_of(&path)));
                } else {
                    self.open_file(&path);
                    self.set_status(format!("created {}", self.editor.rel_of(&path)));
                }
            }
            FileDialog::Delete { target, .. } => {
                if let Err(error) = delete_entry(&target) {
                    self.set_status(format!("delete failed: {error}"));
                    return;
                }
                self.dialog = None;
                self.editor.remove_paths(&target);
                self.tracks
                    .retain(|track| track.path != target && !track.path.starts_with(&target));
                if self
                    .unsupported
                    .as_ref()
                    .is_some_and(|open| open == &target || open.starts_with(&target))
                {
                    self.unsupported = None;
                }
                let parent = target
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.root.clone());
                self.refresh_tree();
                self.tree.reveal(&parent);
                self.focus = Focus::Tree;
                self.set_status(format!("deleted {}", self.editor.rel_of(&target)));
            }
            FileDialog::Provider { fields, .. } => {
                if self.agent.busy {
                    self.set_status("stop the agent before changing providers");
                    return;
                }
                match save_provider_to(&self.models_file, &fields) {
                    Ok(message) => {
                        self.dialog = None;
                        let running = matches!(self.pi, PiState::Running(_));
                        if running {
                            self.restart_pi();
                            self.set_status(format!("{message}, pi restarted"));
                        } else {
                            self.set_status(message);
                        }
                    }
                    Err(error) => self.set_status(error),
                }
            }
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
                self.diff = None;
                self.set_status(message);
                self.reload_after_tool(&path);
                self.refresh_tree();
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
        self.follow_editor_cursor = true;
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
        let suggestions = !self.agent.suggestions.is_empty();
        match key.code {
            KeyCode::PageUp => self.agent.scroll_by(5),
            KeyCode::PageDown => self.agent.scroll_by(-5),
            KeyCode::Esc => {
                if suggestions {
                    self.agent.clear_suggestions();
                } else if self.agent.busy {
                    self.abort_agent();
                }
            }
            KeyCode::Up if suggestions => self.agent.move_suggestion(-1),
            KeyCode::Down if suggestions => self.agent.move_suggestion(1),
            KeyCode::Tab if suggestions => {
                self.agent.accept_suggestion();
            }
            KeyCode::Enter if suggestions => {
                self.agent.accept_suggestion();
            }
            KeyCode::Enter => self.send_prompt(),
            _ if !running => {}
            KeyCode::Backspace => {
                self.agent.backspace_input();
                self.refresh_agent_suggestions();
            }
            KeyCode::Delete => {
                self.agent.delete_input();
                self.refresh_agent_suggestions();
            }
            KeyCode::Left => {
                self.agent.move_input(-1);
                self.refresh_agent_suggestions();
            }
            KeyCode::Right => {
                self.agent.move_input(1);
                self.refresh_agent_suggestions();
            }
            KeyCode::Home => {
                self.agent.input_home();
                self.refresh_agent_suggestions();
            }
            KeyCode::End => {
                self.agent.input_end();
                self.refresh_agent_suggestions();
            }
            KeyCode::Char('u') if ctrl => self.agent.clear_input(),
            KeyCode::Char(ch) if !ctrl && !alt => {
                self.agent.insert_input(ch);
                self.refresh_agent_suggestions();
            }
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

    fn copy_text(&mut self, text: &str, action: &str) {
        self.editor.clipboard = text.to_string();
        match copy_to_clipboard(text) {
            Ok(()) => self.set_status(action),
            Err(_) => self.set_status(format!("{action} to app clipboard")),
        }
    }

    fn clipboard_text(&self) -> Option<String> {
        clipboard_from_system()
            .filter(|text| !text.is_empty())
            .or_else(|| (!self.editor.clipboard.is_empty()).then(|| self.editor.clipboard.clone()))
    }

    fn paste_editor(&mut self) {
        match self.clipboard_text() {
            Some(text) => {
                self.editor.insert_text(&text);
                self.set_status("pasted");
            }
            None => self.set_status("clipboard is empty"),
        }
    }

    fn paste_agent(&mut self) {
        match self.clipboard_text() {
            Some(text) => {
                for ch in text.replace(['\r', '\n'], " ").chars() {
                    self.agent.insert_input(ch);
                }
                self.refresh_agent_suggestions();
                self.set_status("pasted");
            }
            None => self.set_status("clipboard is empty"),
        }
    }

    pub fn open_path(&mut self, path: &Path) {
        self.open_file(path);
    }

    pub fn status_hint(&self, width: u16) -> String {
        let full = width >= 110;
        if let Some(dialog) = &self.dialog {
            return match dialog {
                FileDialog::Delete { .. } => "enter delete  esc cancel",
                FileDialog::Rename { .. } | FileDialog::Create { .. } => {
                    "enter confirm  esc cancel"
                }
                FileDialog::Provider { .. } => "enter save  esc cancel  up/down field",
            }
            .to_string();
        }
        if let Some(history) = &self.history {
            return if history.confirm.is_some() {
                "enter or d delete  esc cancel"
            } else {
                "enter resume  d delete  r refresh  esc close"
            }
            .to_string();
        }
        if let Some(picker) = &self.picker {
            return picker.footer.clone();
        }
        if self.context_menu.is_some() {
            return "up/down select  enter run  esc close".to_string();
        }
        if let Some(menu) = &self.menu {
            return if menu.pages.len() > 1 {
                "enter run  esc back"
            } else {
                "enter open  esc close"
            }
            .to_string();
        }
        if self.diff.is_some() && self.focus == Focus::Editor {
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

        let diff_open = self.diff.is_some();
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
                    if diff_open {
                        "enter commit  esc close".to_string()
                    } else if full {
                        "enter commit  esc cancel".to_string()
                    } else {
                        "enter commit".to_string()
                    }
                }
                Sidebar::Git => {
                    if diff_open {
                        if full {
                            "enter open  esc close  c commit  p push  d discard  r refresh  ctrl+g files"
                        } else {
                            "enter open  esc close  c commit  p push  d discard"
                        }
                    } else if full {
                        "enter diff  o open  c commit  p push  d discard  r refresh  ctrl+g files"
                    } else {
                        "enter diff  c commit  p push  d discard  ctrl+g files"
                    }
                    .to_string()
                }
                Sidebar::Files => {
                    if diff_open {
                        if full {
                            return format!(
                                "enter open  esc close  h/l fold  {sidebar_toggle}  {agent_toggle}  ctrl+q quit"
                            );
                        }
                        return format!("enter open  esc close  {sidebar_toggle}  ctrl+q quit");
                    }
                    if full {
                        return format!(
                            "enter open  right-click menu  h/l fold  {sidebar_toggle}  {agent_toggle}  ctrl+q quit"
                        );
                    }
                    return format!("enter open  right-click menu  {sidebar_toggle}  ctrl+q quit");
                }
            }
            .to_string();
        }

        if self.focus == Focus::Agent {
            if !self.agent.suggestions.is_empty() {
                return if full {
                    "up/down select  tab/enter complete  esc close"
                } else {
                    "tab complete  esc close"
                }
                .to_string();
            }
            return match &self.pi {
                PiState::Running(_) if self.agent.busy => {
                    if full {
                        "enter queue  esc stop  ctrl+b hide"
                    } else {
                        "esc stop  ctrl+b hide"
                    }
                }
                PiState::Running(_) => {
                    if diff_open && full {
                        "enter send  esc close diff  /help commands  ctrl+b hide"
                    } else if diff_open {
                        "enter send  esc close diff  ctrl+b hide"
                    } else if full {
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
                self.follow_editor_cursor = true;
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
        if self.diff.is_some() && self.focus == Focus::Editor {
            return;
        }
        match self.focus {
            Focus::Editor => self.editor.insert_text(text),
            Focus::Agent => {
                let text = text.replace(['\r', '\n'], " ");
                for ch in text.chars() {
                    self.agent.insert_input(ch);
                }
                self.refresh_agent_suggestions();
            }
            Focus::Tree => {}
        }
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let x = mouse.column;
        let y = mouse.row;
        self.hover = Some((x, y));
        if self.dialog.is_some() {
            return;
        }
        if self.history.is_some() {
            self.refresh_hover();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let row = self
                        .ui
                        .history_rows
                        .iter()
                        .find(|(rect, _)| inside(*rect, x, y))
                        .map(|(_, index)| *index);
                    if let Some(index) = row {
                        if let Some(history) = &mut self.history {
                            history.selected = index;
                            history.confirm = None;
                        }
                    } else if !inside(self.ui.history, x, y) {
                        self.history = None;
                    }
                }
                MouseEventKind::ScrollUp => {
                    if let Some(history) = &mut self.history {
                        history.selected = history.selected.saturating_sub(1);
                        history.confirm = None;
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(history) = &mut self.history {
                        history.selected = (history.selected + 1)
                            .min(history.sessions.len().saturating_sub(1));
                        history.confirm = None;
                    }
                }
                _ => {}
            }
            return;
        }
        if self.picker.is_some() {
            self.refresh_hover();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let row = self
                        .ui
                        .picker_rows
                        .iter()
                        .find(|(rect, _)| inside(*rect, x, y))
                        .map(|(_, index)| *index);
                    if let Some(index) = row {
                        let valid = self
                            .picker
                            .as_ref()
                            .is_some_and(|picker| index < picker.items.len());
                        if valid {
                            if let Some(picker) = &mut self.picker {
                                picker.selected = index;
                                picker.touched = true;
                            }
                            self.apply_selected_picker_row();
                        }
                    } else if !inside(self.ui.picker, x, y) {
                        self.picker = None;
                    }
                }
                MouseEventKind::ScrollUp => {
                    if let Some(picker) = &mut self.picker {
                        picker.selected = picker.selected.saturating_sub(1);
                        picker.touched = true;
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(picker) = &mut self.picker {
                        picker.selected = (picker.selected + 1)
                            .min(picker.items.len().saturating_sub(1));
                        picker.touched = true;
                    }
                }
                _ => {}
            }
            return;
        }
        if self.context_menu.is_some() {
            self.refresh_hover();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let rect = self.ui.context_menu;
                    if inside(rect, x, y)
                        && y > rect.y
                        && y + 1 < rect.y.saturating_add(rect.height)
                    {
                        let row = y.saturating_sub(rect.y + 1) as usize;
                        if let Some(menu) = &mut self.context_menu {
                            if row < menu.items.len() {
                                menu.selected = row;
                            }
                        }
                        let action = self
                            .context_menu
                            .as_ref()
                            .and_then(|menu| menu.items.get(row))
                            .map(|item| item.action);
                        if let Some(action) = action {
                            self.run_context_action(action);
                        }
                    } else {
                        self.context_menu = None;
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    if self.sidebar == Sidebar::Files && self.tree_hover_row(x, y).is_some() {
                        let row = y.saturating_sub(self.ui.tree_body.y) as usize;
                        self.tree.select_row(row);
                        self.focus = Focus::Tree;
                        self.open_context_menu(x, y);
                    } else {
                        self.context_menu = None;
                    }
                }
                MouseEventKind::Moved => {
                    let rect = self.ui.context_menu;
                    if inside(rect, x, y)
                        && y > rect.y
                        && y + 1 < rect.y.saturating_add(rect.height)
                    {
                        let row = y.saturating_sub(rect.y + 1) as usize;
                        if let Some(menu) = &mut self.context_menu {
                            if row < menu.items.len() {
                                menu.selected = row;
                            }
                        }
                    }
                }
                MouseEventKind::ScrollUp => {
                    if let Some(menu) = &mut self.context_menu {
                        menu.selected = menu.selected.saturating_sub(1);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(menu) = &mut self.context_menu {
                        menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1));
                    }
                }
                _ => {}
            }
            return;
        }
        if self.menu.is_some() {
            return;
        }
        self.refresh_hover();

        // The diff only owns clicks inside the editor pane. Clicks in the
        // sidebar, the agent pane, or on a divider behave as usual.
        if self.diff.is_some() && inside(self.ui.editor, x, y) {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(diff) = &mut self.diff {
                        diff.scroll = diff.scroll.saturating_sub(3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(diff) = &mut self.diff {
                        diff.scroll = diff.scroll.saturating_add(3);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if inside(self.ui.diff_header, x, y) => {
                    self.diff = None;
                    self.set_status("diff closed");
                }
                _ => {}
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if self.sidebar == Sidebar::Files && self.tree_hover_row(x, y).is_some() {
                    let row = y.saturating_sub(self.ui.tree_body.y) as usize;
                    self.tree.select_row(row);
                    self.focus = Focus::Tree;
                    self.open_context_menu(x, y);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.git.confirm_discard = false;
                self.dragging = false;
                if let Some(edge) = self.resize_edge_at(x, y) {
                    self.resizing = Some(edge);
                    self.hover_edge = Some(edge);
                    self.set_pointer(PointerShape::Resize);
                    return;
                }
                if !inside(self.ui.tree, x, y) {
                    self.last_tree_click = None;
                }
                if inside(self.ui.tree, x, y) {
                    self.focus = Focus::Tree;
                    if self.sidebar == Sidebar::Git {
                        if inside(self.ui.git_input, x, y) {
                            self.git.input_focus = true;
                        } else if y >= self.ui.tree_body.y && self.git_hover_row(x, y).is_some() {
                            self.git
                                .select_row(y.saturating_sub(self.ui.tree_body.y) as usize);
                            self.open_diff();
                        }
                    } else if y >= self.ui.tree_body.y {
                        let hover = self.tree_hover_row(x, y);
                        let double = self.is_double_tree_click(hover);
                        if double && hover.is_none() {
                            self.open_create_dialog(self.root.clone(), false);
                            return;
                        }
                        let row = y.saturating_sub(self.ui.tree_body.y) as usize;
                        self.tree.select_row(row);
                        if let Some(path) = self.tree.activate() {
                            self.open_file(&path);
                        }
                    }
                } else if inside(self.ui.editor, x, y) {
                    self.focus = Focus::Editor;
                    self.follow_editor_cursor = true;
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
                        self.refresh_agent_suggestions();
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
                    self.follow_editor_cursor = false;
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
                    self.follow_editor_cursor = false;
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

        if self.dialog.is_some() {
            self.hover_edge = None;
            self.set_pointer(PointerShape::Default);
            return;
        }

        if self.history.is_some() {
            self.hover_edge = None;
            let over_row = self
                .ui
                .history_rows
                .iter()
                .any(|(rect, _)| inside(*rect, x, y));
            self.set_pointer(if over_row {
                PointerShape::Pointer
            } else {
                PointerShape::Default
            });
            return;
        }

        if self.picker.is_some() {
            self.hover_edge = None;
            let over_row = self
                .ui
                .picker_rows
                .iter()
                .any(|(rect, _)| inside(*rect, x, y));
            self.set_pointer(if over_row {
                PointerShape::Pointer
            } else {
                PointerShape::Default
            });
            return;
        }

        if self.context_menu.is_some() {
            self.hover_edge = None;
            let rect = self.ui.context_menu;
            let row = y.saturating_sub(rect.y + 1) as usize;
            let over_item = inside(rect, x, y)
                && y > rect.y
                && y + 1 < rect.y.saturating_add(rect.height)
                && self
                    .context_menu
                    .as_ref()
                    .is_some_and(|menu| row < menu.items.len());
            self.set_pointer(if over_item {
                PointerShape::Pointer
            } else {
                PointerShape::Default
            });
            return;
        }

        if self.diff.is_some() && inside(self.ui.editor, x, y) {
            // Inside the diff only the title is clickable.
            self.hover_edge = None;
            let over_header = inside(self.ui.diff_header, x, y);
            self.set_pointer(if over_header {
                PointerShape::Pointer
            } else {
                PointerShape::Default
            });
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
            } else if (self.sidebar == Sidebar::Files && self.tree_hover_row(x, y).is_some())
                || (self.sidebar == Sidebar::Git && self.git_hover_row(x, y).is_some())
            {
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

    fn is_double_tree_click(&mut self, row: Option<usize>) -> bool {
        let now = Instant::now();
        let double = self.last_tree_click.is_some_and(|(at, last)| {
            last == row && now.duration_since(at) <= Duration::from_millis(400)
        });
        self.last_tree_click = if double { None } else { Some((now, row)) };
        double
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

fn child_path(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name is empty".to_string());
    }
    if name == "." || name == ".." {
        return Err("name is reserved".to_string());
    }
    if name.contains(['/', '\\', '\0']) {
        return Err("name cannot contain path separators".to_string());
    }
    Ok(parent.join(name))
}

fn remap_path(path: &Path, old: &Path, new: &Path) -> PathBuf {
    path.strip_prefix(old)
        .map(|suffix| {
            if suffix.as_os_str().is_empty() {
                new.to_path_buf()
            } else {
                new.join(suffix)
            }
        })
        .unwrap_or_else(|_| path.to_path_buf())
}

fn delete_entry(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
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

const MAX_TOOL_BODY_LINES: usize = 80;

fn tool_body(name: &str, args: &Value) -> Vec<ToolBodyLine> {
    let mut body = Vec::new();
    match name {
        "write" => {
            if let Some(content) = args.get("content").and_then(Value::as_str) {
                push_body_text(&mut body, content, ToolBodyKind::Text);
            }
        }
        "edit" => {
            let edits: Vec<&Value> = args
                .get("edits")
                .and_then(Value::as_array)
                .map(|edits| edits.iter().collect())
                .unwrap_or_else(|| vec![args]);
            for (index, edit) in edits.iter().enumerate() {
                if edits.len() > 1 {
                    body.push(ToolBodyLine {
                        kind: ToolBodyKind::Meta,
                        text: format!("edit {}", index + 1),
                    });
                }
                if let Some(old) = edit.get("oldText").and_then(Value::as_str) {
                    push_body_text(&mut body, old, ToolBodyKind::Del);
                }
                if let Some(new) = edit.get("newText").and_then(Value::as_str) {
                    push_body_text(&mut body, new, ToolBodyKind::Add);
                }
            }
        }
        _ => {}
    }

    if body.len() > MAX_TOOL_BODY_LINES {
        let remaining = body.len() - MAX_TOOL_BODY_LINES;
        body.truncate(MAX_TOOL_BODY_LINES);
        body.push(ToolBodyLine {
            kind: ToolBodyKind::Meta,
            text: format!("... {remaining} more lines"),
        });
    }
    body
}

fn push_body_text(body: &mut Vec<ToolBodyLine>, text: &str, kind: ToolBodyKind) {
    let prefix = match kind {
        ToolBodyKind::Add => "+",
        ToolBodyKind::Del => "-",
        _ => " ",
    };
    for line in text.lines() {
        body.push(ToolBodyLine {
            kind,
            text: format!("{prefix} {line}"),
        });
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

/// Pi's model list, in the order pi reported it. Entries without a provider
/// or id are unusable for `set_model`, so they are dropped.
fn model_list(models: &[Value]) -> Vec<ModelPref> {
    models.iter().filter_map(ModelPref::from_value).collect()
}

fn model_position(models: &[ModelPref], current: Option<&ModelPref>) -> usize {
    current
        .and_then(|current| models.iter().position(|model| model == current))
        .unwrap_or(0)
}

/// Where smol remembers the last model the user picked.
/// Where smol keeps its small per-user state files (model preference,
/// appended system prompt).
pub(crate) fn config_file(name: &str) -> Option<PathBuf> {
    if let Some(base) = env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(base).join("smol").join(name));
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("smol").join(name))
}

/// pi's agent directory, where `models.json` and the auth store live.
fn agent_dir() -> PathBuf {
    env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".pi").join("agent"))
        })
        .unwrap_or_else(|| PathBuf::from(".pi").join("agent"))
}

fn models_file() -> PathBuf {
    agent_dir().join("models.json")
}

/// The configured providers, newest config first by id.
fn read_providers(path: &Path) -> Result<Vec<ProviderPrefill>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    let Some(providers) = value.get("providers").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    Ok(providers
        .iter()
        .map(|(id, entry)| ProviderPrefill::from_entry(id, entry))
        .collect())
}

/// Merges the form into pi's `models.json`, leaving every other key and
/// provider untouched. Existing files are never clobbered when they cannot be
/// parsed.
fn save_provider_to(path: &Path, fields: &[TextPrompt]) -> Result<String, String> {
    let value = |index: usize| -> String {
        fields
            .get(index)
            .map(|field| field.value.trim().to_string())
            .unwrap_or_default()
    };
    let id = value(0);
    let name = value(1);
    let base_url = value(2);
    let api_key = value(3);
    let api = value(4);
    let model_id = value(5);
    let model_name = value(6);
    let context = value(7);

    if id.is_empty() {
        return Err("provider id is required".to_string());
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("provider id may use letters, digits, - and _".to_string());
    }
    if base_url.is_empty() {
        return Err("base url is required".to_string());
    }
    if model_id.is_empty() {
        return Err("model id is required".to_string());
    }

    let mut root = match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text).map_err(|error| {
            format!(
                "{} is not valid JSON, refusing to overwrite it: {error}",
                path.display()
            )
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let object = root
        .as_object_mut()
        .ok_or_else(|| format!("{} is not a JSON object", path.display()))?;
    let providers = object
        .entry("providers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| "providers is not an object".to_string())?;
    let entry = providers
        .entry(id.clone())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("provider {id} is not an object"))?;

    entry.insert("name".to_string(), json!(if name.is_empty() { id.clone() } else { name }));
    entry.insert("baseUrl".to_string(), json!(base_url));
    if !api_key.is_empty() {
        entry.insert("apiKey".to_string(), json!(api_key));
    }
    let api = if api.is_empty() {
        "openai-completions".to_string()
    } else {
        api
    };
    entry.insert("api".to_string(), json!(api));

    let models = entry
        .entry("models")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| "models is not an array".to_string())?;
    let existing = models
        .iter()
        .position(|model| model.get("id").and_then(Value::as_str) == Some(model_id.as_str()));
    let mut model = existing
        .and_then(|index| models.get(index).cloned())
        .unwrap_or_else(|| json!({}));
    model["id"] = json!(model_id);
    if !model_name.is_empty() {
        model["name"] = json!(model_name);
    }
    if let Ok(tokens) = context.parse::<u64>() {
        model["contextWindow"] = json!(tokens);
    }
    match existing {
        Some(index) => models[index] = model,
        None => models.push(model),
    }

    let text = serde_json::to_string_pretty(&root)
        .map_err(|error| format!("cannot serialize models.json: {error}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(format!("provider {id} saved"))
}

fn load_model_pref(path: &Path) -> Option<ModelPref> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    ModelPref::from_value(&value)
}

fn save_model_pref(path: &Path, pref: &ModelPref) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut data = json!({
        "provider": pref.provider,
        "id": pref.id,
        "name": pref.name,
    });
    if let Some(context) = pref.context_window {
        data["contextWindow"] = json!(context);
    }
    fs::write(path, serde_json::to_string_pretty(&data)?)
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

fn clipboard_from_system() -> Option<String> {
    #[cfg(target_os = "linux")]
    const READERS: &[(&str, &[&str])] = &[
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    #[cfg(target_os = "macos")]
    const READERS: &[(&str, &[&str])] = &[("pbpaste", &[])];
    #[cfg(target_os = "windows")]
    const READERS: &[(&str, &[&str])] = &[(
        "powershell",
        &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
    )];
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    const READERS: &[(&str, &[&str])] = &[];

    for &(program, args) in READERS {
        let Ok(output) = Command::new(program).args(args).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let Ok(text) = String::from_utf8(output.stdout) else {
            continue;
        };
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

fn write_system_clipboard(text: &str) -> bool {
    #[cfg(target_os = "linux")]
    const WRITERS: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    #[cfg(target_os = "macos")]
    const WRITERS: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    const WRITERS: &[(&str, &[&str])] = &[];

    for &(program, args) in WRITERS {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let Some(mut input) = child.stdin.take() else {
            let _ = child.wait();
            continue;
        };
        if input.write_all(text.as_bytes()).is_err() {
            drop(input);
            let _ = child.wait();
            continue;
        }
        drop(input);
        if child.wait().map(|status| status.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if text.len() > 100_000 {
        return Err("selection is too large to copy".into());
    }
    if write_system_clipboard(text) {
        return Ok(());
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
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
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

fn list_sessions(dir: &Path, cwd: &Path) -> Vec<SessionInfo> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        })
        .filter_map(|entry| {
            let path = entry.path();
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort_by_key(|file| std::cmp::Reverse(file.1));
    files.truncate(200);
    files
        .into_iter()
        .filter_map(|(path, modified)| read_session_info(&path, modified))
        .filter(|session| {
            session.cwd.is_empty() || same_path(Path::new(&session.cwd), cwd)
        })
        .collect()
}

fn read_session_info(path: &Path, modified: SystemTime) -> Option<SessionInfo> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut header: Option<String> = None;
    let mut name = None;
    let mut message_count = 0;
    let mut first_message = String::new();

    for line in reader.lines() {
        let Ok(line) = line else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if header.is_none() {
            if kind == "session" {
                header = Some(
                    value
                        .get("cwd")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                );
            }
            continue;
        }
        match kind {
            "session_info" => {
                name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string);
            }
            "message" => {
                message_count += 1;
                if first_message.is_empty() {
                    if let Some(message) = value.get("message") {
                        if message.get("role").and_then(Value::as_str) == Some("user") {
                            let text = message_text(message);
                            if !text.trim().is_empty() {
                                first_message = text;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Some(SessionInfo {
        path: path.to_path_buf(),
        cwd: header?,
        name,
        first_message,
        message_count,
        modified,
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")), true)
    }

    #[test]
    fn git_menu_includes_push() {
        let page = test_app().menu_page(MenuPageKind::Git);
        let push = page
            .items
            .iter()
            .find(|item| matches!(item.action, MenuAction::PushGit))
            .expect("git menu should include push");
        assert_eq!(push.label, "Push");
        assert_eq!(push.hint, "p");
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
    fn available_models_stay_out_of_transcript() {
        let mut app = test_app();
        app.on_pi_record(json!({
            "type": "response",
            "command": "get_available_models",
            "success": true,
            "data": {
                "models": [
                    { "provider": "local", "id": "small" },
                    { "provider": "local", "id": "large" }
                ]
            }
        }));
        assert_eq!(app.models.len(), 2);
        assert!(app.agent.entries.is_empty());
    }

    fn model_pref(provider: &str, id: &str, name: &str) -> ModelPref {
        ModelPref {
            provider: provider.to_string(),
            id: id.to_string(),
            name: name.to_string(),
            context_window: None,
        }
    }

    fn sample_models() -> Vec<Value> {
        json!([
            { "provider": "zai", "id": "glm-4.6", "name": "GLM 4.6" },
            {
                "provider": "anthropic",
                "id": "claude-sonnet-4",
                "name": "Claude Sonnet 4",
                "contextWindow": 200000
            },
            { "provider": "openai", "id": "gpt-5", "name": "GPT-5" }
        ])
        .as_array()
        .cloned()
        .expect("models array")
    }

    /// A stand-in for `pi --mode rpc`: it logs every command it receives and
    /// answers `set_model` the way pi does, with the full model object.
    #[cfg(unix)]
    fn fake_pi(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let log = root.join("pi-rpc.log");
        let script = root.join("fake-pi.sh");
        let body = format!(
            "#!/bin/sh\nLOG='{}'\necho \"ARGS: $@\" >> \"$LOG\"\n{}\n",
            log.display(),
            r#"while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LOG"
  case "$line" in
    *set_model*) printf '%s\n' '{"type":"response","command":"set_model","success":true,"data":{"provider":"anthropic","id":"claude-sonnet-4","name":"Claude Sonnet 4","contextWindow":200000}}' ;;
  esac
done"#
        );
        fs::write(&script, body).expect("write fake pi");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fake pi");
        script
    }

    /// Drains pi output until `check` passes, or gives up after ~3 seconds.
    fn wait_until(app: &mut App, check: impl Fn(&App) -> bool) -> bool {
        for _ in 0..300 {
            app.drain_pi();
            if check(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn model_pref_round_trips_through_disk() {
        let root = temp_root("smol-model-pref");
        let path = root.join("nested").join("model.json");
        let pref = model_pref("anthropic", "claude-sonnet-4", "Claude Sonnet 4");
        save_model_pref(&path, &pref).expect("save model");

        let loaded = load_model_pref(&path).expect("load model");
        assert_eq!(loaded.provider, "anthropic");
        assert_eq!(loaded.id, "claude-sonnet-4");
        assert_eq!(loaded.name, "Claude Sonnet 4");

        fs::write(&path, "not json at all").expect("write garbage");
        assert!(load_model_pref(&path).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn set_model_response_remembers_the_model() {
        let root = temp_root("smol-model-remember");
        let mut app = test_app();
        app.model_pref_file = Some(root.join("model.json"));
        app.model_pref = None;

        app.on_response(&json!({
            "type": "response",
            "command": "set_model",
            "success": true,
            "data": {
                "provider": "anthropic",
                "id": "claude-sonnet-4",
                "name": "Claude Sonnet 4"
            }
        }));

        assert_eq!(app.model.as_deref(), Some("Claude Sonnet 4"));
        let saved = load_model_pref(&root.join("model.json")).expect("saved model");
        assert_eq!(saved.id, "claude-sonnet-4");
        assert_eq!(app.model_current.as_ref(), Some(&saved));
        assert_eq!(app.model_pref.as_ref(), Some(&saved));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn startup_get_state_keeps_the_saved_model() {
        let mut app = test_app();
        let saved = model_pref("anthropic", "claude-sonnet-4", "Claude Sonnet 4");
        app.model_pref = Some(saved.clone());

        app.on_response(&json!({
            "type": "response",
            "command": "get_state",
            "success": true,
            "data": {
                "model": { "provider": "zai", "id": "glm-4.6", "name": "GLM 4.6" }
            }
        }));

        // The pane follows whatever pi reports; the preference stays put.
        assert_eq!(app.model.as_deref(), Some("GLM 4.6"));
        assert_eq!(app.model_pref, Some(saved));
    }

    #[test]
    fn switching_sessions_remembers_the_model_pi_restored() {
        let root = temp_root("smol-model-switch");
        let mut app = test_app();
        app.model_pref_file = Some(root.join("model.json"));
        app.model_pref = Some(model_pref("zai", "glm-4.6", "GLM 4.6"));
        app.model_record_once = true;

        app.on_response(&json!({
            "type": "response",
            "command": "get_state",
            "success": true,
            "data": {
                "model": { "provider": "openai", "id": "gpt-5", "name": "GPT-5" }
            }
        }));

        let saved = load_model_pref(&root.join("model.json")).expect("saved model");
        assert_eq!(saved.provider, "openai");
        assert_eq!(saved.id, "gpt-5");
        assert_eq!(app.model_pref.as_ref(), Some(&saved));
        assert!(!app.model_record_once);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn model_opens_a_list_instead_of_asking_for_a_name() {
        let mut app = test_app();
        app.models = sample_models();
        app.model_current = Some(model_pref("anthropic", "claude-sonnet-4", "Claude Sonnet 4"));

        app.agent.input = "/model".to_string();
        app.send_prompt();

        let picker = app.picker.as_ref().expect("picker open");
        assert_eq!(picker.items.len(), 3);
        // The active model starts out under the cursor.
        assert_eq!(picker.selected, 1);
        assert!(app.status_hint(120).contains("enter switch"));
    }

    fn provider_form(values: [&str; 8]) -> Vec<TextPrompt> {
        PROVIDER_LABELS
            .iter()
            .zip(values)
            .map(|(label, value)| TextPrompt::new(*label, value))
            .collect()
    }

    #[test]
    fn saving_a_provider_keeps_the_rest_of_models_json() {
        let root = temp_root("smol-provider-save");
        let path = root.join("models.json");
        fs::write(
            &path,
            json!({
                "providers": {
                    "other": {
                        "baseUrl": "http://other/v1",
                        "apiKey": "keep-me",
                        "api": "openai-completions",
                        "models": []
                    }
                },
                "custom": 42
            })
            .to_string(),
        )
        .expect("seed models.json");

        let fields = provider_form([
            "newp",
            "New Provider",
            "http://127.0.0.1:9000/v1",
            "secret",
            "openai-completions",
            "fast-model",
            "Fast Model",
            "65536",
        ]);
        let message = save_provider_to(&path, &fields).expect("save provider");
        assert_eq!(message, "provider newp saved");

        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read back"))
            .expect("valid json");
        assert_eq!(saved["custom"], 42, "unrelated keys survive");
        assert_eq!(saved["providers"]["other"]["apiKey"], "keep-me");
        let added = &saved["providers"]["newp"];
        assert_eq!(added["name"], "New Provider");
        assert_eq!(added["baseUrl"], "http://127.0.0.1:9000/v1");
        assert_eq!(added["apiKey"], "secret");
        assert_eq!(added["api"], "openai-completions");
        assert_eq!(added["models"][0]["id"], "fast-model");
        assert_eq!(added["models"][0]["name"], "Fast Model");
        assert_eq!(added["models"][0]["contextWindow"], 65536);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saving_a_provider_updates_the_model_it_already_had() {
        let root = temp_root("smol-provider-update");
        let path = root.join("models.json");
        fs::write(
            &path,
            json!({
                "providers": {
                    "home": {
                        "baseUrl": "http://old/v1",
                        "models": [{ "id": "qwen", "name": "Old name", "contextWindow": 1000 }]
                    }
                }
            })
            .to_string(),
        )
        .expect("seed models.json");

        let fields = provider_form([
            "home",
            "",
            "http://new/v1",
            "",
            "",
            "qwen",
            "New name",
            "262000",
        ]);
        save_provider_to(&path, &fields).expect("save provider");

        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).expect("read back"))
            .expect("valid json");
        let entry = &saved["providers"]["home"];
        assert_eq!(saved["providers"].as_object().expect("providers").len(), 1);
        assert_eq!(entry["models"].as_array().expect("models").len(), 1);
        assert_eq!(entry["models"][0]["name"], "New name");
        assert_eq!(entry["models"][0]["contextWindow"], 262000);
        assert_eq!(entry["baseUrl"], "http://new/v1");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saving_a_provider_refuses_broken_files_and_bad_input() {
        let root = temp_root("smol-provider-guard");
        let path = root.join("models.json");
        fs::write(&path, "{ not json").expect("seed broken file");
        let fields = provider_form(["newp", "", "http://x/v1", "", "", "m", "", ""]);
        assert!(save_provider_to(&path, &fields).is_err());
        assert_eq!(
            fs::read_to_string(&path).expect("read back"),
            "{ not json",
            "a broken file is never overwritten"
        );

        let missing_url = provider_form(["newp", "", "", "", "", "m", "", ""]);
        assert_eq!(
            save_provider_to(&path, &missing_url).unwrap_err(),
            "base url is required"
        );
        let bad_id = provider_form(["has space", "", "http://x/v1", "", "", "m", "", ""]);
        assert!(save_provider_to(&path, &bad_id).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_picker_prefills_the_form_from_models_json() {
        let root = temp_root("smol-provider-pick");
        let mut app = test_app();
        app.models_file = root.join("models.json");
        fs::write(
            &app.models_file,
            json!({
                "providers": {
                    "bbb": {
                        "name": "Second",
                        "baseUrl": "http://b/v1",
                        "apiKey": "kb",
                        "api": "openai-completions",
                        "models": [{ "id": "mb", "name": "Model B", "contextWindow": 32768 }]
                    },
                    "aaa": {
                        "name": "First",
                        "baseUrl": "http://a/v1",
                        "apiKey": "ka",
                        "api": "openai-completions",
                        "models": [{ "id": "ma", "name": "Model A", "contextWindow": 128000 }]
                    }
                }
            })
            .to_string(),
        )
        .expect("seed models.json");

        app.open_provider_picker();
        let picker = app.picker.as_ref().expect("picker open");
        assert_eq!(picker.items.len(), 3, "new provider plus two saved ones");
        assert_eq!(picker.items[0].label, "new provider");
        assert_eq!(picker.items[1].label, "First");
        assert_eq!(picker.items[2].label, "Second");

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.picker.is_none(), "the form replaces the picker");
        let Some(FileDialog::Provider { fields, .. }) = &app.dialog else {
            panic!("provider form open");
        };
        assert_eq!(fields[0].value, "aaa");
        assert_eq!(fields[1].value, "First");
        assert_eq!(fields[2].value, "http://a/v1");
        assert_eq!(fields[3].value, "ka");
        assert_eq!(fields[4].value, "openai-completions");
        assert_eq!(fields[5].value, "ma");
        assert_eq!(fields[6].value, "Model A");
        assert_eq!(fields[7].value, "128000");
        assert!(app.status_hint(120).contains("enter save"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn provider_form_saves_and_closes() {
        let root = temp_root("smol-provider-form");
        let mut app = test_app();
        app.models_file = root.join("models.json");
        app.open_provider_form(ProviderPrefill {
            api: "openai-completions".to_string(),
            ..ProviderPrefill::default()
        });

        let values = [
            "myprov",
            "My Prov",
            "http://127.0.0.1:8080/v1",
            "secret",
            "openai-completions",
            "fast",
            "Fast",
            "65536",
        ];
        for value in values {
            // Ctrl+U clears whatever the form was prefilled with.
            app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            for ch in value.chars() {
                app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.dialog.is_none(), "the form closes on save");
        let status = app
            .status
            .as_ref()
            .map(|(text, _)| text.clone())
            .unwrap_or_default();
        assert!(status.contains("provider myprov saved"), "got: {status}");

        let saved: Value = serde_json::from_str(
            &fs::read_to_string(&app.models_file).expect("models.json written"),
        )
        .expect("valid json");
        assert_eq!(
            saved["providers"]["myprov"]["baseUrl"],
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(saved["providers"]["myprov"]["models"][0]["id"], "fast");
        assert_eq!(
            saved["providers"]["myprov"]["models"][0]["contextWindow"],
            65536
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn picker_keys_move_and_escape_closes() {
        let mut app = test_app();
        app.models = sample_models();
        app.open_model_picker();

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.picker.as_ref().map(|picker| picker.selected), Some(2));
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.picker.as_ref().map(|picker| picker.selected), Some(1));
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(app.picker.as_ref().map(|picker| picker.selected), Some(2));
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.picker.as_ref().map(|picker| picker.selected), Some(0));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.picker.is_none());
    }

    #[test]
    fn thinking_opens_a_list_and_sets_the_level() {
        let mut app = test_app();
        app.thinking_level = Some("medium".to_string());
        app.thinking_levels = vec!["off", "low", "medium", "high"]
            .iter()
            .map(|level| level.to_string())
            .collect();

        app.agent.input = "/thinking".to_string();
        app.send_prompt();

        let picker = app.picker.as_ref().expect("picker open");
        assert_eq!(picker.items.len(), 4);
        assert_eq!(picker.selected, 2, "the current level starts selected");
        assert!(picker.items[2].active);
        assert!(app.status_hint(120).contains("enter set"));

        // pi narrows the list to what this model supports.
        app.on_response(&json!({
            "type": "response",
            "command": "get_available_thinking_levels",
            "success": true,
            "data": { "levels": ["off", "medium", "high"] }
        }));
        let picker = app.picker.as_ref().expect("picker open");
        assert_eq!(picker.items.len(), 3);
        assert_eq!(picker.selected, 1, "stays on the current level");

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.picker.is_none());
        assert_eq!(app.thinking_level.as_deref(), Some("high"));
    }

    #[test]
    fn thinking_level_follows_get_state() {
        let mut app = test_app();
        app.on_response(&json!({
            "type": "response",
            "command": "get_state",
            "success": true,
            "data": { "thinkingLevel": "xhigh" }
        }));
        assert_eq!(app.thinking_level.as_deref(), Some("xhigh"));
    }

    #[cfg(unix)]
    #[test]
    fn picking_a_model_sends_set_model_and_remembers_it() {
        let root = temp_root("smol-model-picker-rpc");
        let script = fake_pi(&root);
        let mut app = test_app();
        app.model_pref_file = Some(root.join("model.json"));
        app.model_pref = None;
        app.models = sample_models();
        app.pi = PiState::Running(PiClient::spawn_with(&script, &root, &[]).expect("spawn fake pi"));

        app.open_model_picker();
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.picker.is_none(), "picker closes after choosing");

        let log = root.join("pi-rpc.log");
        let reached_pi = wait_until(&mut app, |_| {
            fs::read_to_string(&log).unwrap_or_default().contains("set_model")
        });
        assert!(reached_pi, "set_model reaches pi");

        let remembered = wait_until(&mut app, |app| app.model_pref.is_some());
        assert!(remembered, "pi's answer is remembered");
        let sent = fs::read_to_string(&log).unwrap_or_default();
        assert!(sent.contains("\"modelId\":\"claude-sonnet-4\""), "got: {sent}");
        let saved = load_model_pref(&root.join("model.json")).expect("saved model");
        assert_eq!(saved.id, "claude-sonnet-4");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn todo_file_drives_the_panel_state() {
        let root = temp_root("smol-todo");
        let mut app = App::new(root.clone(), true);
        let file = root.join(".smol").join("todo.md");
        fs::create_dir_all(file.parent().expect("todo dir")).expect("mkdir");

        fs::write(&file, "# plan\n[ ] first\n[>] second\n").expect("write todo");
        app.tick();
        assert_eq!(app.todo.tasks.len(), 2);
        assert_eq!(app.todo.done(), 0);
        assert_eq!(app.todo.active_index(), Some(1));

        fs::write(&file, "[x] first\n[x] second\n").expect("finish todo");
        app.refresh_todo();
        assert_eq!(app.todo.done(), 2);

        fs::remove_file(&file).expect("remove todo");
        app.refresh_todo();
        assert!(app.todo.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn pi_starts_with_full_permissions_and_no_extensions() {
        let root = temp_root("smol-pi-flags");
        let script = fake_pi(&root);
        let append = root.join("append.md");
        fs::write(&append, "extra instructions").expect("write prompt");
        let mut app = test_app();
        app.pi_path = Some(script);
        app.instructions = Some(append.clone());

        app.start_pi(None);

        let log = root.join("pi-rpc.log");
        let started = wait_until(&mut app, |_| {
            fs::read_to_string(&log).unwrap_or_default().contains("ARGS:")
        });
        assert!(started, "pi started");
        let args = fs::read_to_string(&log).unwrap_or_default();
        assert!(args.contains("--mode rpc"), "got: {args}");
        assert!(args.contains("-a -ne -ns"), "got: {args}");
        assert!(
            args.contains(&format!("--append-system-prompt {}", append.display())),
            "got: {args}"
        );
        assert!(!args.contains("-e "), "no extensions loaded: {args}");
        assert!(!args.contains("--skill"), "no skills loaded: {args}");

        // Restarting resumes the open session instead of losing it.
        let session = root.join("session.jsonl");
        fs::write(&session, "").expect("write session");
        app.session_path = Some(session.clone());
        app.restart_pi();
        let resumed = wait_until(&mut app, |_| {
            fs::read_to_string(&log)
                .unwrap_or_default()
                .matches("ARGS:")
                .count()
                >= 2
        });
        assert!(resumed, "pi restarted");
        let args = fs::read_to_string(&log).unwrap_or_default();
        assert!(
            args.contains(&format!("--session {}", session.display())),
            "got: {args}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn new_session_reapplies_the_last_model() {
        let root = temp_root("smol-model-new-session");
        let script = fake_pi(&root);
        let mut app = test_app();
        app.model_pref_file = Some(root.join("model.json"));
        app.model_pref = Some(model_pref("anthropic", "claude-sonnet-4", "Claude Sonnet 4"));
        app.pi = PiState::Running(PiClient::spawn_with(&script, &root, &[]).expect("spawn fake pi"));

        app.on_response(&json!({
            "type": "response",
            "command": "new_session",
            "success": true
        }));

        let log = root.join("pi-rpc.log");
        let reapplied = wait_until(&mut app, |_| {
            fs::read_to_string(&log).unwrap_or_default().contains("set_model")
        });
        assert!(reapplied, "the saved model is re-sent for a fresh session");
        let sent = fs::read_to_string(&log).unwrap_or_default();
        assert!(sent.contains("\"modelId\":\"claude-sonnet-4\""), "got: {sent}");
        let confirmed = wait_until(&mut app, |app| !app.model_auto_apply);
        assert!(confirmed, "pi confirmed the automatic switch");
        let saved = load_model_pref(&root.join("model.json")).expect("saved model");
        assert_eq!(saved.id, "claude-sonnet-4");
        let _ = fs::remove_dir_all(root);
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

    #[test]
    fn tree_refresh_picks_up_new_files_and_folders() {
        let root = std::env::temp_dir().join(format!("smol-tree-refresh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test project");
        let mut app = App::new(root.clone(), true);

        fs::create_dir(root.join("new")).expect("create folder");
        fs::write(root.join("new/file.txt"), "hello").expect("write file");
        app.last_tree_refresh = Instant::now() - Duration::from_secs(3);
        app.tick();

        assert!(app.tree.nodes.iter().any(|node| node.name == "new"));
        assert!(app.project_files.iter().any(|path| path == "new/file.txt"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn right_click_opens_explorer_menu_with_file_actions() {
        let mut app = test_app();
        let readme = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
        app.tree.reveal(&readme);
        let position = app
            .tree
            .visible
            .iter()
            .position(|index| app.tree.nodes[*index].path == readme)
            .expect("README row");
        app.tree.scroll = position.saturating_sub(5);
        app.ui.tree_body = Rect {
            x: 0,
            y: 1,
            width: 30,
            height: 20,
        };
        let y = 1 + (position - app.tree.scroll) as u16;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 2,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
        let menu = app.context_menu.as_ref().expect("context menu");
        assert_eq!(menu.target, readme);
        assert!(!menu.is_dir);
        for action in [
            ContextAction::Open,
            ContextAction::OpenDefault,
            ContextAction::NewFile,
            ContextAction::NewFolder,
            ContextAction::Rename,
            ContextAction::Delete,
            ContextAction::CopyPath,
            ContextAction::CopyRelativePath,
        ] {
            assert!(menu.items.iter().any(|item| item.action == action));
        }
    }

    #[test]
    fn rename_action_updates_editor_buffer_and_tree() {
        let root = std::env::temp_dir().join(format!(
            "smol-context-rename-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test project");
        let old = root.join("old.txt");
        fs::write(&old, "hello").expect("write file");
        let mut app = App::new(root.clone(), true);
        app.open_path(&old);
        app.tree.reveal(&old);

        app.open_context_menu(1, 1);
        app.run_context_action(ContextAction::Rename);
        match &mut app.dialog {
            Some(FileDialog::Rename { prompt, .. }) => {
                prompt.value = "new.txt".to_string();
                prompt.cursor = prompt.value.chars().count();
            }
            _ => panic!("rename dialog missing"),
        }
        app.submit_dialog();

        let new = root.join("new.txt");
        assert!(!old.exists());
        assert!(new.exists());
        assert_eq!(
            app.editor.buffer().map(|buffer| buffer.path.clone()),
            Some(fs::canonicalize(&new).expect("canonical new path"))
        );
        assert!(app.tree.nodes.iter().any(|node| node.path == new));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delete_action_removes_file_and_open_buffer() {
        let root = std::env::temp_dir().join(format!(
            "smol-context-delete-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test project");
        let file = root.join("doomed.txt");
        fs::write(&file, "bye").expect("write file");
        let opened = fs::canonicalize(&file).expect("canonical path");
        let mut app = App::new(root.clone(), true);
        app.open_path(&file);
        app.tree.reveal(&file);

        app.open_context_menu(1, 1);
        app.run_context_action(ContextAction::Delete);
        assert!(matches!(app.dialog, Some(FileDialog::Delete { .. })));
        app.submit_dialog();

        assert!(!file.exists());
        assert!(app.dialog.is_none());
        assert!(app.editor.find_buffer(&opened).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn new_folder_action_creates_folder_in_selected_directory() {
        let root = std::env::temp_dir().join(format!(
            "smol-context-create-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test project");
        let seed = root.join("seed.txt");
        fs::write(&seed, "seed").expect("write seed");
        let mut app = App::new(root.clone(), true);
        app.tree.reveal(&seed);

        app.open_context_menu(1, 1);
        app.run_context_action(ContextAction::NewFolder);
        match &mut app.dialog {
            Some(FileDialog::Create { prompt, .. }) => {
                prompt.value = "notes".to_string();
                prompt.cursor = prompt.value.chars().count();
            }
            _ => panic!("create dialog missing"),
        }
        app.submit_dialog();

        assert!(root.join("notes").is_dir());
        assert!(app.tree.nodes.iter().any(|node| node.name == "notes"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn at_reference_completes_project_file() {
        let mut app = test_app();
        app.agent.input = "inspect @app".to_string();
        app.agent.input_cursor = app.agent.input.chars().count();
        app.refresh_agent_suggestions();
        assert_eq!(
            app.agent.suggestions.first().map(String::as_str),
            Some("src/app.rs")
        );

        assert!(app.agent.accept_suggestion());
        assert_eq!(app.agent.input, "inspect @src/app.rs ");
        assert_eq!(app.agent.input_cursor, app.agent.input.chars().count());
        assert!(app.agent.suggestions.is_empty());
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn write_session(dir: &Path, file: &str, cwd: &Path, name: Option<&str>) -> PathBuf {
        fs::create_dir_all(dir).expect("create session dir");
        let path = dir.join(file);
        let mut content = String::new();
        content.push_str(
            &json!({
                "type": "session",
                "version": 3,
                "id": "session-id",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "cwd": cwd.to_string_lossy().to_string()
            })
            .to_string(),
        );
        content.push('\n');
        if let Some(name) = name {
            content.push_str(
                &json!({
                    "type": "session_info",
                    "id": "name-id",
                    "parentId": null,
                    "timestamp": "2026-01-01T00:00:01.000Z",
                    "name": name
                })
                .to_string(),
            );
            content.push('\n');
        }
        for (id, role, text) in [
            ("m1", "user", Some("first prompt")),
            ("m2", "assistant", Some("the reply")),
            ("m3", "toolResult", None),
        ] {
            let mut message = json!({ "role": role, "timestamp": 1 });
            match (role, text) {
                ("user", Some(text)) | ("assistant", Some(text)) => {
                    message["content"] = json!([{ "type": "text", "text": text }]);
                }
                ("toolResult", _) => {
                    message["toolCallId"] = json!("call-1");
                    message["toolName"] = json!("bash");
                    message["isError"] = json!(false);
                    message["content"] = json!([]);
                }
                _ => {}
            }
            content.push_str(
                &json!({
                    "type": "message",
                    "id": id,
                    "parentId": null,
                    "timestamp": "2026-01-01T00:00:02.000Z",
                    "message": message
                })
                .to_string(),
            );
            content.push('\n');
        }
        fs::write(&path, content).expect("write session");
        path
    }

    #[test]
    fn session_history_lists_named_sessions_for_this_project() {
        let root = temp_root("smol-session-list");
        let dir = root.join("sessions");
        let file = write_session(&dir, "one.jsonl", &root, Some("my work"));
        write_session(&dir, "other.jsonl", Path::new("/some/other/project"), None);

        let sessions = list_sessions(&dir, &root);
        assert_eq!(sessions.len(), 1);
        let session = sessions.first().expect("session");
        assert_eq!(session.path, file);
        assert_eq!(session.name.as_deref(), Some("my work"));
        assert_eq!(session.message_count, 3);
        assert_eq!(session.first_message, "first prompt");
        assert_eq!(session.label(), "my work");
        assert!(!session.age().is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resume_history_rebuilds_transcript_from_messages() {
        let mut app = test_app();
        let messages = json!([
            { "role": "user", "content": [{ "type": "text", "text": "question" }] },
            {
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "plan" },
                    {
                        "type": "toolCall",
                        "id": "call-1",
                        "name": "bash",
                        "arguments": { "command": "ls" }
                    }
                ]
            },
            {
                "role": "toolResult",
                "toolCallId": "call-1",
                "toolName": "bash",
                "isError": true,
                "content": []
            },
            { "role": "assistant", "content": [{ "type": "text", "text": "answer" }] }
        ]);
        app.load_transcript(messages.as_array().expect("messages"));

        assert!(matches!(
            app.agent.entries.first(),
            Some(Entry::User(text)) if text == "question"
        ));
        assert!(matches!(
            app.agent.entries.get(1),
            Some(Entry::Assistant { text, thinking, .. })
                if text.is_empty() && thinking == "plan"
        ));
        assert!(matches!(
            app.agent.entries.get(2),
            Some(Entry::Tool(tool)) if tool.state == ToolState::Failed
        ));
        assert!(matches!(
            app.agent.entries.get(3),
            Some(Entry::Assistant { text, .. }) if text == "answer"
        ));
        assert_eq!(app.agent.entries.len(), 4);
    }

    #[test]
    fn history_keys_resume_and_delete_sessions() {
        let root = temp_root("smol-session-history");
        let mut app = App::new(root.clone(), true);
        let dir = root.join("sessions");
        let file = write_session(&dir, "one.jsonl", &root, Some("work"));
        app.session_dir = Some(dir.clone());
        app.open_history();

        let history = app.history.as_ref().expect("history open");
        assert_eq!(history.sessions.len(), 1);
        assert_eq!(history.selected, 0);
        assert!(app.status_hint(120).contains("enter resume"));

        app.handle_history_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.history.is_some(), "resume stays open without pi");
        assert!(app.pending_switch.is_none());
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(message, _)| message == "pi is not installed"));

        app.handle_history_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(
            app.history.as_ref().and_then(|history| history.confirm.as_ref()),
            Some(&file)
        );
        app.handle_history_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(!file.exists());
        assert!(app
            .history
            .as_ref()
            .is_some_and(|history| history.sessions.is_empty()));

        app.handle_history_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.history.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "spawns a real pi process; run with cargo test -- --ignored"]
    fn end_to_end_history_resume_with_pi() {
        if crate::pi::find_pi().is_none() {
            return;
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut app = App::new(root, false);
        for _ in 0..200 {
            app.drain_pi();
            if app.session_path.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(app.session_path.is_some(), "pi should report a session file");

        app.open_history();
        let Some(path) = app
            .history
            .as_ref()
            .expect("history open")
            .sessions
            .first()
            .map(|session| session.path.clone())
        else {
            app.shutdown();
            return;
        };

        app.resume_session(path.clone());
        for _ in 0..200 {
            app.drain_pi();
            if app.pending_switch.is_none() && !app.agent.entries.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(app.pending_switch.is_none(), "switch should complete");
        assert_eq!(app.session_path.as_deref(), Some(path.as_path()));
        assert!(
            !app.agent.entries.is_empty(),
            "resumed transcript should load"
        );
        app.shutdown();
    }

    fn make_git_repo(prefix: &str) -> (PathBuf, PathBuf) {
        fn git(dir: &Path, args: &[&str]) {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
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

        let base = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let repo = base.join("repo");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&repo).expect("create repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "smol@example.com"]);
        git(&repo, &["config", "user.name", "smol test"]);
        fs::write(repo.join("note.txt"), "one\n").expect("write note");
        fs::write(repo.join("extra.txt"), "alpha\n").expect("write extra");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "initial"]);
        fs::write(repo.join("note.txt"), "one\ntwo\n").expect("update note");
        fs::write(repo.join("extra.txt"), "alpha\nbeta\n").expect("update extra");
        (base, repo)
    }

    #[test]
    fn diff_view_keeps_git_workflow_usable() {
        let (base, repo) = make_git_repo("smol-diff-commit");
        let mut app = App::new(repo, true);
        app.handle_key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        ));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.diff.is_some(), "change should open its diff");

        // typing a commit message works while the diff is open
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.git.input_focus, "commit input should focus");
        for ch in "add queue".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(app.git.input, "add queue");
        assert!(app.status_hint(120).contains("enter commit"));

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            app.git.changes.is_empty(),
            "commit should run while the diff is open"
        );
        assert!(!app.git.input_focus);
        assert!(app.diff.is_none(), "the stale diff closes after a commit");

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn diff_view_only_takes_over_the_editor_keys() {
        let (base, repo) = make_git_repo("smol-diff-keys");
        let mut app = App::new(repo, true);
        let lines = (0..40)
            .map(|index| DiffLine {
                kind: DiffKind::Context,
                text: format!("line {index}"),
            })
            .collect();
        app.diff = Some(DiffView {
            title: "M  note.txt".to_string(),
            lines,
            scroll: 0,
        });

        // with the editor focused the diff scrolls and owns the keys
        app.focus = Focus::Editor;
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.diff.as_ref().map(|diff| diff.scroll), Some(1));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.diff.is_none());

        // with the sidebar focused the list keeps its keys
        app.diff = Some(DiffView {
            title: "M  note.txt".to_string(),
            lines: (0..40)
                .map(|index| DiffLine {
                    kind: DiffKind::Context,
                    text: format!("line {index}"),
                })
                .collect(),
            scroll: 0,
        });
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        let selected = app.git.selected;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_ne!(app.git.selected, selected, "list should move");
        assert_eq!(
            app.diff.as_ref().map(|diff| diff.scroll),
            Some(0),
            "the diff should not scroll"
        );

        // global shortcuts keep working too
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(app.agent_hidden, "ctrl+b should still work");
        app.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert_ne!(app.focus, Focus::Tree, "f6 should still cycle focus");
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.diff.is_none(), "esc should close the diff");

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn ctrl_r_opens_and_esc_closes_session_history() {
        let root = temp_root("smol-ctrl-r");
        let mut app = App::new(root.clone(), true);
        app.session_dir = Some(root.join("sessions"));

        app.handle_key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        ));
        assert!(app.history.is_some());
        assert!(app
            .history
            .as_ref()
            .is_some_and(|history| history.sessions.is_empty()));
        assert!(app.status_hint(120).contains("esc close"));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.history.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn switch_session_response_updates_active_path() {
        let mut app = test_app();
        let path = PathBuf::from("/tmp/smol-test-session.jsonl");
        app.pending_switch = Some(path.clone());

        app.on_response(&json!({
            "type": "response",
            "command": "switch_session",
            "success": true,
            "data": { "cancelled": false }
        }));

        assert_eq!(app.session_path.as_ref(), Some(&path));
        assert_eq!(
            app.session_dir.as_deref(),
            path.parent().map(Path::new)
        );
        assert!(app.pending_switch.is_none());
        assert!(app.status.as_ref().is_some_and(|(text, _)| text == "loading session"));
    }
}
