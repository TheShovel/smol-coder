//! The instructions smol appends to pi's own system prompt.
//!
//! smol starts pi with `--no-extensions --no-skills`, so nothing the user has
//! installed as a skill or extension is loaded. What those would have supplied
//! (the testing discipline, the todo workflow, local search instead of exa)
//! lives here instead, and pi keeps its default prompt underneath.

use std::fs;
use std::path::{Path, PathBuf};

/// Path of the file smol writes for `--append-system-prompt`, if the platform
/// gives us somewhere to put it.
pub fn instructions_file() -> Option<PathBuf> {
    crate::app::config_file("agent-append.md")
}

/// Writes [`APPEND_PROMPT`] to [`instructions_file`] when it is missing or
/// outdated and returns the path pi should read. Also installs the local web
/// helpers the prompt points at. Returns `None` when the file cannot be
/// written, so pi never runs with a stale copy of the prompt.
pub fn write_instructions() -> Option<PathBuf> {
    write_helpers();
    let path = instructions_file()?;
    write_instructions_to(&path).then_some(path)
}

/// Writes [`APPEND_PROMPT`] to `path`, skipping the write when the file
/// already holds the current text. Returns false when the file cannot be
/// written.
pub fn write_instructions_to(path: &Path) -> bool {
    write_text_if_changed(path, &append_prompt())
}

/// The prompt pi receives: `APPEND_PROMPT` with the helper paths filled in.
pub fn append_prompt() -> String {
    APPEND_PROMPT
        .replace("{{WEBSEARCH}}", &helper_command("websearch"))
        .replace("{{WEBFETCH}}", &helper_command("webfetch"))
}

fn helpers_dir() -> Option<PathBuf> {
    crate::app::config_file("bin")
}

fn helper_command(name: &str) -> String {
    helpers_dir()
        .map(|dir| dir.join(name))
        .unwrap_or_else(|| PathBuf::from("~/.config/smol/bin").join(name))
        .display()
        .to_string()
}

/// Installs `websearch` and `webfetch` next to the config, executable so the
/// model can run them with pi's `bash` tool.
fn write_helpers() -> bool {
    let Some(dir) = helpers_dir() else {
        return false;
    };
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    write_helper(&dir.join("websearch"), crate::scripts::WEBSEARCH)
        && write_helper(&dir.join("webfetch"), crate::scripts::WEBFETCH)
}

fn write_helper(path: &Path, body: &str) -> bool {
    if fs::read_to_string(path).ok().as_deref() != Some(body)
        && fs::write(path, body).is_err()
    {
        return false;
    }
    make_executable(path)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).is_ok()
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> bool {
    true
}

fn write_text_if_changed(path: &Path, text: &str) -> bool {
    if fs::read_to_string(path).ok().as_deref() == Some(text) {
        return true;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    fs::create_dir_all(parent).is_ok() && fs::write(path, text).is_ok()
}

pub const APPEND_PROMPT: &str = r#"# Added by smol

smol runs pi with `--no-extensions` and `--no-skills`. No skills, extension
commands, MCP servers, or image tools are available in this session; web search
does not come from an extension either - it comes from the local scripts
described under Searching. Everything else you would normally get from them is
written below, and it stays in force for the whole session.

## Todo list

Track any request that needs more than two steps in `.smol/todo.md` in the
project root. smol renders that file in the agent pane. The progress bar
automatically calculates the percentage based on completed vs total tasks,
so you don't need to manually update it. One task per line. The
first three characters are the status and everything after them is the task
title:

```text
[ ] write the script
[>] run the failing test
[x] update the readme
```

`[ ]` means not started, `[>]` means this is what you are working on right
now, `[x]` means finished. Write real task titles after the marker - never the
words "pending", "in progress", or "done".

Rules:

- Create the file with `write` before you start working, then keep it current.
- Exactly one task may be `[>]`. Mark a task `[>]` the moment you start it and
  `[x]` the moment it really finishes - after the check passed, not before.
- Rewrite the whole file with `write` on every change and keep the plan order.
  Split, add, or drop tasks when the plan changes.
- Keep tasks small and concrete: "run the failing test", not "fix the bug".
- When the work is done and the user moves on, delete `.smol/todo.md` so the
  panel clears.

## Testing

You are a small model. Your intuition is not evidence. Test in small steps,
read the actual output, and only then report a result.

Hard rules:

- Never say "done", "fixed", "works", or "all tests pass" until a real command
  produced that result.
- Use the project's own language, package manager, and test framework. Do not
  invent a test harness or add a dependency for a check the project can do.
- A build, compile, or typecheck proves the code builds. It does not prove
  behavior. Run the behavior.
- Test the requested behavior and at least one failure path. A happy path alone
  is not enough.
- When something fails, fix the cause. Never delete, skip, loosen, or mock away
  a test just to get green.
- If a check cannot run, say exactly what was not tested and why. Never report
  it as passing.
- Re-run the affected check after your last edit. Earlier passing runs do not
  cover later changes.

Workflow:

1. Put the plan in `.smol/todo.md` before the first read or edit.
2. Discover the tooling first: `ls`, then `package.json`, `Cargo.toml`,
   `pyproject.toml`, `setup.cfg`, `go.mod`, `Makefile`, and any README or
   AGENTS file. Find the real install, lint, typecheck, and test commands.
3. Turn the request into three to six observable checks, each passable or
   failable from output, not from opinion.
4. Run the smallest focused check first, then the broader suite, then the
   production build if there is one. Read failures completely; do not hide the
   exit status behind `head` or `tail`.
5. When the app has a UI, start it the way a user would, interact with it, and
   read the logs it keeps. There are no screenshot or image tools here, so
   never claim a screen "looks right": say visual verification was not
   performed unless real output proves the behavior.
6. Check the failure surface: invalid input, missing files, error paths, and
   empty states must fail loudly, not crash.
7. Report the exact commands you ran and what they printed. Name the gap if
   something could not be tested.

## Searching

Search this machine first: `grep` for file contents, `find` for paths, `ls` for
listings, `read` for files. Most answers live in the project.

For the web, smol installs two scripts that run on this machine - no exa, no
search service, no API key. Call them with the `bash` tool:

- `{{WEBSEARCH}} rust async runtime` - numbered results, each with title, url,
  and snippet. `-n 12` widens the result count.
- `{{WEBFETCH}} https://example.com/page` - a page as plain text. Read a result
  before you quote it.

Rules:

- Go to the web only when the answer is not on this machine, and say which
  query you searched. Never claim you searched the web when you did not.
- Cite the urls you actually fetched. If a script fails, report the failure
  instead of guessing an answer.
- A `FAILED:` line or an empty page is a dead end: never fetch the same url
  twice, never retry the same query forever - move on, try a different angle,
  or answer with what you already have.
- The scripts pick a public engine themselves; `~/.config/smol/search.json`
  points them at your own instance instead, for example
  `{"searxng": "http://localhost:8888"}`.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn temp_file() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "smol-instructions-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn instructions_write_idempotently() {
        let path = temp_file();
        assert!(write_instructions_to(&path));
        let first = fs::read_to_string(&path).expect("prompt file");
        assert_eq!(first, append_prompt());

        // An outdated copy is replaced, an up-to-date one is left alone.
        fs::write(&path, "stale").expect("stale prompt");
        assert!(write_instructions_to(&path));
        assert_eq!(
            fs::read_to_string(&path).expect("prompt file"),
            append_prompt()
        );
        assert!(write_instructions_to(&path));

        let _ = fs::remove_dir_all(&path);
    }

    #[test]
    fn prompt_covers_the_replaced_extensions_and_skills() {
        let rendered = append_prompt();
        for needle in [
            ".smol/todo.md",
            "[>] run the failing test",
            "grep",
            "Never say \"done\"",
            "--no-extensions",
            "websearch",
            "webfetch",
            "searxng",
            "dead end",
        ] {
            assert!(
                rendered.contains(needle),
                "prompt is missing {needle:?}"
            );
        }
        assert!(!APPEND_PROMPT.to_lowercase().contains("vision"));

        assert!(!rendered.contains("{{"), "placeholders stay substituted");
        assert!(
            rendered.contains(&helper_command("websearch")),
            "the prompt names the real helper path"
        );
        assert!(
            !rendered.contains("no web search"),
            "web search is local now, not missing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_search_helpers_are_installed_and_runnable() {
        assert!(write_helpers(), "helpers installed");
        let dir = helpers_dir().expect("helpers dir");

        let python = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok();
        if !python {
            eprintln!("python3 missing, only checked the files exist");
        }

        for name in ["websearch", "webfetch"] {
            let path = dir.join(name);
            let body = fs::read_to_string(&path).expect("helper written");
            assert!(body.starts_with("#!/usr/bin/env python3"), "{name}");

            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("helper metadata").permissions().mode();
            assert_ne!(mode & 0o111, 0, "{name} must be executable");

            if python {
                // Parse without writing any bytecode next to the helper.
                let parsed = std::process::Command::new("python3")
                    .args(["-c", "import ast,sys; ast.parse(open(sys.argv[1]).read())"])
                    .arg(&path)
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false);
                assert!(parsed, "{name} must be valid python");

                // Running it with no query prints usage and exits 2.
                let status = std::process::Command::new(&path)
                    .status()
                    .expect("run helper");
                assert_eq!(status.code(), Some(2), "{name} usage exit code");
            }
        }

        // The prompt is regenerated with the paths that were just written.
        let rendered = append_prompt();
        assert!(rendered.contains(&dir.join("websearch").display().to_string()));
    }
}
