mod app;
mod editor;
mod git;
mod highlight;
mod markdown;
mod pi;
mod predict;
mod theme;
mod tree;
mod ui;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::panic;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use crate::app::{App, PointerShape};

fn main() {
    if let Err(error) = run() {
        eprintln!("smol: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut force_no_pi = false;
    let mut root: Option<PathBuf> = None;

    for argument in env::args().skip(1) {
        match argument.as_str() {
            "--force-no-pi" => force_no_pi = true,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("smol {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}\nrun smol --help"));
            }
            other => {
                if root.is_some() {
                    return Err("only one path can be opened".into());
                }
                root = Some(PathBuf::from(other));
            }
        }
    }

    let target = match root {
        Some(path) => path,
        None => {
            env::current_dir().map_err(|error| format!("cannot read current folder: {error}"))?
        }
    };
    let (root, initial_file) = resolve_target(target)?;

    let mut app = App::new(root, force_no_pi);
    if let Some(path) = initial_file {
        app.open_path(&path);
    }

    if let Err(error) = enable_raw_mode() {
        app.shutdown();
        return Err(format!("cannot start terminal: {error}"));
    }
    if let Err(error) = execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    ) {
        let _ = disable_raw_mode();
        app.shutdown();
        return Err(format!("cannot start terminal: {error}"));
    }

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous_hook(info);
    }));

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            restore_terminal();
            app.shutdown();
            return Err(format!("cannot start terminal: {error}"));
        }
    };
    let _ = terminal.clear();
    set_pointer(PointerShape::Default);

    let result = run_loop(&mut terminal, &mut app);

    app.shutdown();
    restore_terminal();
    let _ = terminal.show_cursor();
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<(), String> {
    loop {
        app.drain_pi();
        app.tick();
        app.refresh_hover();
        if let Some(shape) = app.take_pointer_update() {
            set_pointer(shape);
        }
        terminal
            .draw(|frame| ui::draw(frame, app))
            .map_err(|error| format!("draw failed: {error}"))?;

        if app.should_quit {
            return Ok(());
        }

        let ready = event::poll(Duration::from_millis(80))
            .map_err(|error| format!("terminal read failed: {error}"))?;
        if !ready {
            continue;
        }

        match event::read().map_err(|error| format!("terminal read failed: {error}"))? {
            Event::Key(key) => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            Event::Paste(text) => app.handle_paste(&text),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn resolve_target(path: PathBuf) -> Result<(PathBuf, Option<PathBuf>), String> {
    let path = fs::canonicalize(&path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    if path.is_dir() {
        return Ok((path, None));
    }
    if path.is_file() {
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", path.display()))?
            .to_path_buf();
        return Ok((parent, Some(path)));
    }
    Err(format!("{} is not a file or folder", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_file_to_parent_and_file() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let (root, file) = resolve_target(path).expect("resolve file");
        let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("project root");
        assert_eq!(root, expected);
        assert_eq!(
            file.expect("file path")
                .file_name()
                .expect("file name")
                .to_string_lossy(),
            "README.md"
        );
    }

    #[test]
    fn resolves_folder_without_file() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let (root, file) = resolve_target(path).expect("resolve folder");
        assert!(root.is_dir());
        assert!(file.is_none());
    }
}

fn set_pointer(shape: PointerShape) {
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]22;{}\x1b\\", shape.css());
    let _ = out.flush();
}

fn restore_terminal() {
    set_pointer(PointerShape::Default);
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        Show,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}

fn print_help() {
    println!(
        "smol {} - a small terminal IDE for the pi coding agent

Usage:
  smol [path]
  smol --force-no-pi [path]

Path can be a folder or a file. Text files open directly in the editor. Images, sound, and other binary files show a button to open them in the default app.

Options:
  --force-no-pi   Show the missing pi state even when pi is installed
  -h, --help      Show this help
  -V, --version   Show the version

Keys:
  F6              Cycle files, editor, agent
  Alt+1/2/3       Focus files, editor, agent
  Ctrl+G          Toggle the files and git sidebar
  Ctrl+B          Hide or show the agent pane
  Ctrl+S          Save the current file
  Ctrl+Z          Undo the last edit
  Ctrl+Q          Quit
  Ctrl+P          Open the command menu
  Ctrl+N          Next open file
  Ctrl+W          Close the current file
  Ctrl+C          Copy the editor selection
  Ctrl+X          Cut the editor selection
  Ctrl+V          Paste into the editor or agent prompt
  Shift+arrows    Select text
  Tab             Accept a prediction or @ file suggestion, or indent
  BackTab         Dedent
  Enter           Open a file or diff, send a prompt, or commit
  @               Reference a project file in the agent prompt
  Esc             Stop the agent or close the diff

Mouse:
  Click           Open a file, folder, or diff; click a tab; focus a pane; place the cursor
  Hover           Change the pointer shape and highlight interactive targets
  Drag divider    Resize the sidebar or agent pane

Agent commands:
  /help           List commands and pi resources
  /new            Start a new session
  /compact        Compact context
  /model          List or switch models
  /thinking       Show or set the thinking level
  /abort          Stop the running prompt
  /stats          Show session usage
  /copy           Copy the last assistant message
  /hide, /show    Hide or show the agent pane

Git sidebar:
  c               Write a commit message, Enter commits
  p               Push
  d               Discard the selected change, press twice to confirm
  r               Refresh status
  Enter           Open the selected diff
  o               Open the selected file",
        env!("CARGO_PKG_VERSION")
    );
}
