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

The file tree also supports arrows and `h`, `j`, `k`, `l`. It refreshes automatically when files and folders change, and open buffers close themselves when their file is deleted. Double-click empty space in the tree to create a file. Click files and folders, drag pane dividers to resize, and drag in the editor to select text. Right-click an explorer entry for Open, Open in default app, New file, New folder, Rename, Delete, Copy path, and Copy relative path. Delete asks for confirmation and permanently removes the file or folder. Cut and copy use the local clipboard tool when available, otherwise the terminal's OSC 52 clipboard support; paste reads the system clipboard and falls back to text cut or copied in smol.

Type `@` in the agent input to reference a project file. A list filters paths as you type; use Up/Down and Tab or Enter to insert one.

The command menu groups actions into `Files`, `Settings`, `LLM commands`, and `Git` submenus. The `Git` submenu refreshes status and pushes. `Settings` also opens the model picker (`/model`), the thinking-level picker (`/thinking`), and the connect-provider form.

## pi

smol starts pi as a subprocess in the project folder:

```bash
pi --mode rpc -a -ne -ns --append-system-prompt ~/.config/smol/agent-append.md
```

- `-a` trusts the project for this run, so pi starts with full permissions and
  never blocks on a trust prompt.
- `-ne -ns` disables every extension and skill: no `/todos`, `/kb`, `/ollama`,
  or `/skill:*` commands, no exa or vision tools, no MCP servers. Web search
  still works, from smol's own local helpers (below).
- `--append-system-prompt` adds smol's own instructions on top of pi's default
  prompt. smol writes that file to `~/.config/smol/agent-append.md` at startup
  (or `$XDG_CONFIG_HOME/smol/agent-append.md`) and rewrites it when the content
  changes. It carries what the disabled extensions and skills used to provide:
  the testing discipline, the todo workflow, and how to search.

### Web search

Search stays local: instead of the exa extension, smol installs two
dependency-free Python scripts next to its config and points the appended
prompt at them:

```bash
~/.config/smol/bin/websearch -n 8 "rust ownership"
~/.config/smol/bin/webfetch https://docs.rs/tokio/
```

`websearch` queries public engines from this machine (Bing, DuckDuckGo lite,
Mojeek - whichever answers first) and prints numbered titles, urls, and
snippets; `webfetch` reads a page as plain text, or prints `FAILED:` when it
cannot. Both take `-n` for size, need no API key, and write nothing. Point
`~/.config/smol/search.json` at a local SearXNG instance
(`{"searxng": "http://localhost:8888"}`) to search through your own instead.
The prompt tells the agent to search the project with `grep` and `find` first,
to cite the urls it actually fetched, and to treat a failed page as a dead end
instead of retrying it.

pi manages authentication, models, and sessions. Sessions are saved by pi under `~/.pi/agent/sessions/`, so the agent pane keeps a history you can reopen: `Ctrl+R`, `/history`, or the command menu's `Session history` entry lists saved sessions for the current project, with `Enter` to resume one and `d` to delete it (confirm with `d` or `Enter`).

Model, thinking, and provider are all controlled from smol, so there is no need to drop into pi's terminal UI:

- `/model` or the menu's `Model` entry opens a selectable list of every model pi
  reports, marked with the active one; `Enter` switches. The last model you pick
  is remembered in `~/.config/smol/model.json` and restored on the next launch
  and on every new session, so smol stops defaulting back to GLM. `/model
  provider/model` still switches directly.
- `/thinking` or the menu's `Thinking` entry lists the levels the current model
  supports; `Enter` sets one.
- The menu's `Provider` entry lists the providers in `~/.pi/agent/models.json`
  with a `new provider` row. Picking one opens a form (id, name, base url, api
  key, api, model id, model name, context window); `Enter` saves it, merging
  into the file without touching anything else in it, and restarts pi with the
  open session resumed.

The model keeps a todo list at `.smol/todo.md` in the project (one `[ ]`, `[>]`,
or `[x]` task per line, described in the appended prompt). smol renders it above
the agent input: a `todos 2/5` heading with a progress bar, every task with what
is done, and how long each task took - live while it runs, final once it is
finished.

If pi is missing, the agent pane links to its installer. Type `/help` in the agent input for available commands. The command menu's `LLM commands` submenu can explain or review the current file, explain or refactor a selection, add tests, and format the file. Commands that edit a file require it to be saved first.

smol checks context usage after each run and compacts automatically at 95%. pi's built-in auto-compaction is disabled so smol controls the threshold; `/compact` still compacts manually.

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

Commits stage all changes. Diffs open in the editor and support keyboard and mouse scrolling. Press `Esc` (or `q`) or click the diff title to close it; while a diff is open the sidebar, the commit input, and the agent prompt keep their keys, so you can still browse changes, commit, or type a prompt.

## Editing

Inline predictions are local and instant. They learn from open files, a bounded project-wide corpus, saved line transitions, and accepted predictions. The engine uses the current line suffix, the previous line, indentation, and nearby line pairs. On an indented empty line it suggests the next line; `Tab` accepts. Before anything has been learned, a built-in set of common syntax and words (Rust, JavaScript/TypeScript, Python, Go, C/C++, and shell) provides predictions; these are only a fallback, so your own code and learned lines always take priority. Learned lines and transitions are stored in `~/.cache/smol/`. No network request, model download, or pi subprocess is involved.

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

Remove the `# smol path` block from your shell startup file if the installer added it.
