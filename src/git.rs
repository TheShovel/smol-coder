use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Instant;

#[derive(Clone)]
pub struct Change {
    pub status: String,
    pub path: String,
    pub orig: Option<String>,
}

pub struct Git {
    pub root: PathBuf,
    pub available: bool,
    pub repo: bool,
    pub branch: String,
    pub changes: Vec<Change>,
    pub selected: usize,
    pub scroll: usize,
    pub input: String,
    pub input_cursor: usize,
    pub input_focus: bool,
    pub confirm_discard: bool,
    pub pushing: bool,
    pub last_refresh: Instant,
    push_rx: Option<Receiver<Result<String, String>>>,
}

impl Git {
    pub fn new(root: &PathBuf) -> Self {
        let available = Command::new("git")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        let repo = available && run_git(root, &["rev-parse", "--is-inside-work-tree"]).is_ok();

        let mut git = Self {
            root: root.clone(),
            available,
            repo,
            branch: String::new(),
            changes: Vec::new(),
            selected: 0,
            scroll: 0,
            input: String::new(),
            input_cursor: 0,
            input_focus: false,
            confirm_discard: false,
            pushing: false,
            last_refresh: Instant::now(),
            push_rx: None,
        };
        if repo {
            git.refresh();
        }
        git
    }

    pub fn refresh(&mut self) {
        if !self.repo {
            return;
        }
        let branch = run_git(&self.root, &["branch", "--show-current"]).unwrap_or_default();
        self.branch = if branch.is_empty() {
            "detached".to_string()
        } else {
            branch
        };

        match Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["status", "--porcelain=v1", "-z"])
            .output()
        {
            Ok(output) if output.status.success() => {
                self.changes = parse_status(&output.stdout);
                if self.selected >= self.changes.len() {
                    self.selected = self.changes.len().saturating_sub(1);
                }
            }
            _ => {}
        }
        self.last_refresh = Instant::now();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.changes.is_empty() {
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, self.changes.len().saturating_sub(1) as isize) as usize;
    }

    pub fn select_row(&mut self, row: usize) {
        let index = self.scroll + row;
        if index < self.changes.len() {
            self.selected = index;
        }
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.changes
            .get(self.selected)
            .map(|change| self.root.join(&change.path))
    }

    pub fn selected_change(&self) -> Option<&Change> {
        self.changes.get(self.selected)
    }

    pub fn commit(&mut self) -> Result<String, String> {
        let message = self.input.trim().to_string();
        if message.is_empty() {
            return Err("commit message is empty".into());
        }
        run_git(&self.root, &["add", "-A"])?;
        let result = run_git(&self.root, &["commit", "-m", &message]);
        self.input.clear();
        self.input_cursor = 0;
        self.input_focus = false;
        self.refresh();
        match result {
            Ok(output) => Ok(output.lines().next().unwrap_or("committed").to_string()),
            Err(error) => Err(error),
        }
    }

    pub fn push(&mut self) -> Result<(), String> {
        if !self.repo {
            return Err("not a git repository".into());
        }
        if self.pushing {
            return Err("push already running".into());
        }
        let root = self.root.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = run_git(&root, &["push"]).map(|_| "pushed".to_string());
            let _ = tx.send(result);
        });
        self.push_rx = Some(rx);
        self.pushing = true;
        Ok(())
    }

    pub fn poll_push(&mut self) -> Option<Result<String, String>> {
        let receiver = self.push_rx.as_ref()?;
        match receiver.try_recv() {
            Ok(result) => {
                self.push_rx = None;
                self.pushing = false;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.push_rx = None;
                self.pushing = false;
                Some(Err("push stopped".into()))
            }
        }
    }

    pub fn diff_selected(&self) -> Result<(String, String), String> {
        let change = self
            .selected_change()
            .cloned()
            .ok_or("no change selected")?;
        let title = format!("{}  {}", change.status.trim(), change.path);
        let untracked = change.status.starts_with("??") || change.status.contains('?');

        let text = if untracked {
            let path = self.root.join(&change.path);
            if path.is_dir() {
                String::new()
            } else {
                let content = fs::read_to_string(&path)
                    .map_err(|error| format!("cannot read {}: {error}", change.path))?;
                content
                    .lines()
                    .map(|line| format!("+{line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        } else {
            match run_git(
                &self.root,
                &["diff", "--no-color", "HEAD", "--", &change.path],
            ) {
                Ok(text) if !text.is_empty() => text,
                _ => run_git(&self.root, &["diff", "--no-color", "--", &change.path])?,
            }
        };

        Ok((title, text))
    }

    pub fn discard_selected(&mut self) -> Result<(String, PathBuf), String> {
        let change = self
            .selected_change()
            .cloned()
            .ok_or("no change selected")?;
        let path = self.root.join(&change.path);
        let untracked = change.status.starts_with("??") || change.status.contains('?');

        if untracked {
            let result = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = result {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("discard failed: {error}"));
                }
            }
        } else {
            let mut paths: Vec<&str> = Vec::new();
            if let Some(orig) = &change.orig {
                paths.push(orig.as_str());
            }
            paths.push(change.path.as_str());

            let mut args = vec!["restore", "--staged", "--worktree", "--"];
            args.extend(paths.iter().copied());
            if run_git(&self.root, &args).is_err() {
                let mut args = vec!["checkout", "--"];
                args.extend(paths.iter().copied());
                run_git(&self.root, &args)?;
            }
        }

        self.confirm_discard = false;
        self.refresh();
        Ok((format!("discarded {}", change.path), path))
    }

    pub fn insert_input(&mut self, ch: char) {
        let mut chars: Vec<char> = self.input.chars().collect();
        let col = self.input_cursor.min(chars.len());
        chars.insert(col, ch);
        self.input = chars.into_iter().collect();
        self.input_cursor = col + 1;
    }

    pub fn backspace_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let mut chars: Vec<char> = self.input.chars().collect();
        let col = self.input_cursor.min(chars.len());
        chars.remove(col - 1);
        self.input = chars.into_iter().collect();
        self.input_cursor = col - 1;
    }

    pub fn delete_input(&mut self) {
        let mut chars: Vec<char> = self.input.chars().collect();
        if self.input_cursor < chars.len() {
            chars.remove(self.input_cursor);
            self.input = chars.into_iter().collect();
        }
    }

    pub fn move_input(&mut self, delta: isize) {
        let length = self.input.chars().count();
        let next = self.input_cursor as isize + delta;
        self.input_cursor = next.clamp(0, length as isize) as usize;
    }

    pub fn input_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn input_end(&mut self) {
        self.input_cursor = self.input.chars().count();
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }
}

fn run_git(root: &PathBuf, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("git: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let text = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        let line = text.lines().next().unwrap_or("git command failed");
        Err(line.to_string())
    }
}

fn parse_status(bytes: &[u8]) -> Vec<Change> {
    let tokens: Vec<&[u8]> = bytes
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty())
        .collect();
    let mut changes = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        let token = tokens[index];
        index += 1;
        if token.len() < 4 {
            continue;
        }
        let x = token[0] as char;
        let y = token[1] as char;
        let path = String::from_utf8_lossy(&token[3..]).to_string();

        let mut orig = None;
        if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            if let Some(previous) = tokens.get(index) {
                orig = Some(String::from_utf8_lossy(previous).to_string());
                index += 1;
            }
        }

        changes.push(Change {
            status: format!("{x}{y}"),
            path,
            orig,
        });
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
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
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn commit_discard_and_push_local() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("git-test-{}", std::process::id()));
        let repo = base.join("repo");
        let remote = base.join("remote.git");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&repo).unwrap();

        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "smol@example.com"]);
        git(&repo, &["config", "user.name", "smol test"]);
        fs::write(repo.join("note.txt"), "one\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "initial"]);
        git(&base, &["init", "--bare", "-q", "remote.git"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-q", "-u", "origin", "HEAD"]);

        fs::write(repo.join("note.txt"), "one\ntwo\n").unwrap();
        let mut manager = Git::new(&repo);
        assert!(manager.repo);
        assert!(!manager.changes.is_empty());
        let (_, diff) = manager.diff_selected().expect("diff");
        assert!(diff.contains("+two"));

        manager.input = "second".to_string();
        manager.commit().expect("commit");
        assert!(manager.changes.is_empty());

        fs::write(repo.join("note.txt"), "one\ntwo\nthree\n").unwrap();
        manager.refresh();
        manager.discard_selected().expect("discard");
        assert_eq!(
            fs::read_to_string(repo.join("note.txt")).unwrap(),
            "one\ntwo\n"
        );

        manager.push().expect("start push");
        let mut pushed = None;
        for _ in 0..60 {
            if let Some(result) = manager.poll_push() {
                pushed = Some(result);
                break;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(matches!(pushed, Some(Ok(_))), "push did not complete");

        let _ = fs::remove_dir_all(&base);
    }
}
