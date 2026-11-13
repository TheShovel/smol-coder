# smol

smol is a small terminal IDE for the pi coding agent. It provides a file tree, a syntax-highlighted editor, a git view, and an agent pane.

![Screenie](images/screenie.png)

It is built to stay small: the Linux x86_64 release binary is 4.1 MB, and an idle session uses about 5.9 MiB of RSS and 0.2% CPU without a pi subprocess.

## Requirements

- Rust 1.74 or newer (installed automatically if missing)
- git
- pi for the agent pane

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/TheShovel/smol-coder/main/install.sh | bash
```

The script downloads and builds smol, installs it to `~/.local/bin/smol`, and adds that folder to PATH when needed. It installs Rust with rustup if `cargo` is missing.

## Run

```bash
smol
smol path/to/project
smol path/to/file
```

Run `smol --help` for all options.

## Keys

| Key | Action |
| --- | --- |
| `F6` | Cycle panes |
| `Alt+1`, `Alt+2`, `Alt+3` | Focus files, editor, or agent |
| `Ctrl+G` | Switch files and git sidebar |
| `Ctrl+B` | Hide or show the agent pane |
| `Ctrl+R` | Open session history to resume or delete a session |
| `Ctrl+S` | Save |
| `Ctrl+Z` | Undo |
| `Ctrl+Q` | Quit |
| `Ctrl+C` | Copy selection |
| `Ctrl+X` | Cut selection |
| `Ctrl+V` | Paste into the editor or agent prompt |
| `Ctrl+N` | Next file |
| `Ctrl+P` | Command menu and settings |
| `Ctrl+W` | Close file |
| `Ctrl+Left`, `Ctrl+Right` | Move by word |
| `Ctrl+Home`, `Ctrl+End` | Start or end of file |
| `Shift+arrows` | Select text |
| `Tab` | Accept a prediction or @ file suggestion, or indent |
| `BackTab` | Dedent |
| `Enter` | Open a file or diff, send a prompt, pick a row, or commit |
| `Esc` | Stop the agent, or close a diff, picker, or dialog |

The file tree also supports arrows and `h`, `j`, `k`, `l`.

Type `@` in the agent input to reference a project file. A list filters paths as you type; use Up/Down and Tab or Enter to insert one.

Agent reads and edits are highlighted in the editor. The editor follows the active file unless you have unsaved changes there.

## Git

The git sidebar shows changed files. Click a change to open its diff in the editor.

| Key | Action |
| --- | --- |
| `c` | Write a commit message |
| `p` | Push |
| `d` | Discard, press twice to confirm |
| `r` | Refresh |
| `o` | Open the selected file |

## Editing

Inline predictions are local and instant. They learn from open files, a bounded project-wide corpus, saved line transitions, and accepted predictions. The engine uses the current line suffix, the previous line, indentation, and nearby line pairs. On an indented empty line it suggests the next line; `Tab` accepts. Before anything has been learned, a built-in set of common syntax and words (Rust, JavaScript/TypeScript, Python, Go, C/C++, and shell) provides predictions.

Binary files and files over 4 MB are not opened in the editor. Unsupported files show a button to open them with the system default application.

## Development

```bash
cargo build
cargo test
cargo run -- --force-no-pi
```

## Uninstall

```bash
rm ~/.local/bin/smol
rm -rf ~/.cache/smol
rm -rf ~/.config/smol
```

`~/.config/smol` holds the remembered model and the appended system prompt;
remove it only if you also want smol to forget them.
