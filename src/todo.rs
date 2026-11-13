//! The todo list smol keeps in `<project>/.smol/todo.md`.
//!
//! The model rewrites that file with `write`; smol parses it, times each task,
//! and renders the panel in the agent pane. The markers here are the same ones
//! the prompt asks the model to write, so the file stays human readable.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TodoStatus {
    Pending,
    Active,
    Done,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TodoTask {
    pub title: String,
    pub status: TodoStatus,
}

/// Reads `[ ]`, `[>]`, and `[x]` task lines. Everything else in the file
/// (headings, notes, blank lines) is ignored, so a half-written file can never
/// break the panel.
pub fn parse(text: &str) -> Vec<TodoTask> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line);
            let status = match rest.get(..3)? {
                "[ ]" => TodoStatus::Pending,
                "[>]" => TodoStatus::Active,
                "[x]" | "[X]" => TodoStatus::Done,
                _ => return None,
            };
            let title = rest[3..].trim();
            if title.is_empty() {
                return None;
            }
            Some(TodoTask {
                title: title.to_string(),
                status,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct Timing {
    created: Instant,
    started: Option<Instant>,
    finished: Option<Instant>,
}

pub struct TodoList {
    pub tasks: Vec<TodoTask>,
    timings: HashMap<String, Timing>,
    pub last_poll: Option<Instant>,
    /// Consecutive empty reads; one empty read while the model may be halfway
    /// through rewriting the file must not clear the panel.
    empty_reads: u8,
}

impl TodoList {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            timings: HashMap::new(),
            last_poll: None,
            empty_reads: 0,
        }
    }

    /// Applies a fresh read of the todo file, tolerating a single empty read
    /// (the model is mid-rewrite) before it clears the panel.
    pub fn apply_read(&mut self, tasks: Vec<TodoTask>, now: Instant) {
        if tasks.is_empty() && !self.tasks.is_empty() {
            self.empty_reads = self.empty_reads.saturating_add(1);
            if self.empty_reads < 2 {
                return;
            }
        } else if !tasks.is_empty() {
            self.empty_reads = 0;
        }
        self.apply(tasks, now);
    }

    /// Replaces the visible list with a fresh parse and records the status
    /// transitions that show how long each task took.
    pub fn apply(&mut self, tasks: Vec<TodoTask>, now: Instant) {
        let mut seen: HashSet<&str> = HashSet::new();
        for task in &tasks {
            if !seen.insert(task.title.as_str()) {
                continue;
            }
            let timing = self
                .timings
                .entry(task.title.clone())
                .or_insert(Timing {
                    created: now,
                    started: None,
                    finished: None,
                });
            match task.status {
                TodoStatus::Pending => {
                    timing.started = None;
                    timing.finished = None;
                }
                TodoStatus::Active => {
                    if timing.started.is_none() {
                        timing.started = Some(now);
                    }
                    timing.finished = None;
                }
                TodoStatus::Done => {
                    if timing.finished.is_none() {
                        timing.finished = Some(now);
                    }
                }
            }
        }
        self.tasks = tasks;
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn done(&self) -> usize {
        self.tasks
            .iter()
            .filter(|task| task.status == TodoStatus::Done)
            .count()
    }

    pub fn active_index(&self) -> Option<usize> {
        self.tasks
            .iter()
            .position(|task| task.status == TodoStatus::Active)
    }

    /// How long the task has been at its current stage: a finished task shows
    /// its full duration, the running one ticks live, a pending one shows
    /// nothing.
    pub fn elapsed(&self, title: &str, now: Instant) -> Option<Duration> {
        let timing = self.timings.get(title)?;
        let base = timing.started.unwrap_or(timing.created);
        match self.tasks.iter().find(|task| task.title == title) {
            Some(TodoTask {
                status: TodoStatus::Done,
                ..
            }) => timing
                .finished
                .map(|finished| finished.saturating_duration_since(base)),
            Some(TodoTask {
                status: TodoStatus::Active,
                ..
            }) => Some(now.saturating_duration_since(base)),
            _ => None,
        }
    }
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(title: &str, status: TodoStatus) -> TodoTask {
        TodoTask {
            title: title.to_string(),
            status,
        }
    }

    #[test]
    fn parses_only_task_lines() {
        let tasks = parse(
            "# plan\n\n[ ] first step\n- [>] second step\n* [X] third step\n[ ] \nnote\n",
        );
        assert_eq!(
            tasks,
            vec![
                task("first step", TodoStatus::Pending),
                task("second step", TodoStatus::Active),
                task("third step", TodoStatus::Done),
            ]
        );
    }

    #[test]
    fn ignores_a_file_without_tasks() {
        assert!(parse("").is_empty());
        assert!(parse("just some notes\n## headings\n").is_empty());
        assert!(parse("[?] unknown marker\n").is_empty());
    }

    #[test]
    fn times_each_task_across_transitions() {
        let start = Instant::now();
        let mut list = TodoList::new();

        list.apply(vec![task("one", TodoStatus::Pending)], start);
        assert_eq!(list.elapsed("one", start), None);

        let active = start + Duration::from_secs(10);
        list.apply(vec![task("one", TodoStatus::Active)], active);
        // The clock starts when the task does, not when it was written down.
        assert_eq!(list.elapsed("one", active), Some(Duration::from_secs(0)));
        assert_eq!(
            list.elapsed("one", start + Duration::from_secs(25)),
            Some(Duration::from_secs(15))
        );

        let done = start + Duration::from_secs(45);
        list.apply(vec![task("one", TodoStatus::Done)], done);
        // 35s of work: the 10s it spent pending does not count.
        assert_eq!(list.elapsed("one", done), Some(Duration::from_secs(35)));

        // The panel keeps the final figure while nothing changes.
        assert_eq!(
            list.elapsed("one", start + Duration::from_secs(60)),
            Some(Duration::from_secs(35))
        );
        assert_eq!(list.done(), 1);
        assert_eq!(list.active_index(), None);
    }

    #[test]
    fn a_task_marked_done_without_running_shows_its_whole_life() {
        let start = Instant::now();
        let mut list = TodoList::new();
        list.apply(vec![task("quick", TodoStatus::Pending)], start);
        let done = start + Duration::from_secs(4);
        list.apply(vec![task("quick", TodoStatus::Done)], done);
        assert_eq!(list.elapsed("quick", done), Some(Duration::from_secs(4)));
    }

    #[test]
    fn reverting_a_task_clears_its_stops() {
        let start = Instant::now();
        let mut list = TodoList::new();
        list.apply(vec![task("one", TodoStatus::Active)], start);
        list.apply(vec![task("one", TodoStatus::Done)], start + Duration::from_secs(5));
        list.apply(vec![task("one", TodoStatus::Active)], start + Duration::from_secs(9));
        let now = start + Duration::from_secs(12);
        // The restart keeps the original start, so the clock does not reset.
        assert_eq!(list.elapsed("one", now), Some(Duration::from_secs(12)));
        assert_eq!(list.active_index(), Some(0));
    }

    #[test]
    fn a_single_empty_read_does_not_clear_the_panel() {
        let now = Instant::now();
        let mut list = TodoList::new();
        list.apply_read(vec![task("one", TodoStatus::Pending)], now);

        // The file is empty for one poll: the model is rewriting it.
        list.apply_read(Vec::new(), now);
        assert_eq!(list.tasks.len(), 1);

        // Two empty reads in a row mean it is really gone.
        list.apply_read(Vec::new(), now);
        assert!(list.is_empty());
    }

    #[test]
    fn a_missing_file_clears_the_panel() {
        let now = Instant::now();
        let mut list = TodoList::new();
        list.apply_read(vec![task("one", TodoStatus::Pending)], now);
        list.apply(Vec::new(), now);
        assert!(list.is_empty());
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(format_duration(Duration::from_secs(9)), "9s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m05s");
        assert_eq!(format_duration(Duration::from_secs(3_725)), "1h02m");
    }
}
