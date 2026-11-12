use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const MAX_LEARNED: usize = 3000;
const MAX_TRANSITIONS: usize = 4000;
const MAX_PROJECT_LINES: usize = 30_000;
const MAX_TAIL: usize = 120;
const MAX_LINE: usize = 400;
const MAX_KEY: usize = 20;
const MIN_SCORE: u32 = 8;

const IGNORED: &[&str] = &[".git", "node_modules", "target", ".freebuff", ".smol"];

// Built-in suggestions so a fresh install is useful before anything is learned.
// These are only used when the user's own lines, the open file, and the project
// corpus have no match, so real code always wins over these defaults.
const DEFAULT_WEIGHT: u32 = 3;
const DEFAULT_LINES: &[&str] = &[
    // General syntax
    "return;",
    "return value;",
    "return None;",
    "return null;",
    "return Ok(value);",
    "return Err(error);",
    "let value = 0;",
    "let mut value = 0;",
    "let value = String::new();",
    "let value = Vec::new();",
    "let value = HashMap::new();",
    "let Some(value) = option else {",
    "const NAME: &str = \"\";",
    "static NAME: &str = \"\";",
    "if condition {",
    "} else if condition {",
    "} else {",
    "for item in items {",
    "for (index, item) in items.iter().enumerate() {",
    "while condition {",
    "loop {",
    "match value {",
    "Some(value) => value,",
    "None => return,",
    "Ok(value) => value,",
    "Err(error) => error,",
    "struct Name {",
    "enum Name {",
    "trait Name {",
    "impl Name {",
    "pub struct Name {",
    "pub fn new() -> Self {",
    "pub fn name(&self) -> &str {",
    "pub fn name(&self) -> String {",
    "fn main() {",
    "println!(\"Hello, world!\");",
    "println!(\"{value:?}\");",
    "eprintln!(\"{error}\");",
    "vec![1, 2, 3]",
    "String::from(\"value\")",
    "value.to_string()",
    "value.as_str()",
    "value.clone()",
    "Option<String>",
    "Result<(), Box<dyn std::error::Error>>",
    "use std::collections::HashMap;",
    "use std::fs;",
    "use std::io;",
    "use std::path::{Path, PathBuf};",
    "use std::sync::Arc;",
    "#[derive(Debug, Clone)]",
    "#[cfg(test)]",
    "#[test]",
    "assert_eq!(left, right);",
    "assert!(condition);",
    "unimplemented!()",
    "todo!()",
    // JavaScript and TypeScript
    "function name() {",
    "async function name() {",
    "const name = () => {",
    "export default function name() {",
    "export function name() {",
    "export default name;",
    "import { name } from \"module\";",
    "import name from \"module\";",
    "console.log(value);",
    "console.error(error);",
    "await promise;",
    "let name = value;",
    "const name = value;",
    "const name = [];",
    "const name = {};",
    "if (condition) {",
    "for (const item of items) {",
    "for (let index = 0; index < items.length; index++) {",
    "try {",
    "} catch (error) {",
    "interface Name {",
    "type Name = {",
    "class Name {",
    "constructor() {",
    "public name: string;",
    "private name: string;",
    "document.querySelector(\"selector\");",
    "addEventListener(\"click\", () => {",
    "JSON.stringify(value)",
    "Object.keys(value)",
    "Array.isArray(value)",
    "Promise.all(items)",
    // Python
    "def name():",
    "def __init__(self):",
    "def main():",
    "class Name:",
    "import os",
    "import sys",
    "import json",
    "from pathlib import Path",
    "from typing import Optional",
    "if __name__ == \"__main__\":",
    "print(value)",
    "for item in items:",
    "for index, item in enumerate(items):",
    "if condition:",
    "elif condition:",
    "else:",
    "try:",
    "except Exception as error:",
    "with open(path) as file:",
    "self.name = value",
    "raise ValueError(\"message\")",
    "value = []",
    "value = {}",
    // Go
    "func name() {",
    "func main() {",
    "if err != nil {",
    "for index, item := range items {",
    "return nil, err",
    "return value, nil",
    "fmt.Println(value)",
    "fmt.Errorf(\"error: %w\", err)",
    "type Name struct {",
    "type Name interface {",
    "value := 0",
    "var value int",
    "defer file.Close()",
    // C and C++
    "int main() {",
    "return 0;",
    "#include <stdio.h>",
    "std::cout << value << std::endl;",
    "printf(\"%d\\n\", value);",
    "for (int index = 0; index < count; index++) {",
    // Shell
    "echo \"$value\"",
    "if [ -f \"$file\" ]; then",
    "for file in *.txt; do",
    "export NAME=value",
    "set -euo pipefail",
];

// Built-in next-line suggestions, again used only as a fallback.
const DEFAULT_TRANSITIONS: &[(&str, &str)] = &[
    ("fn main() {", "    println!(\"Hello, world!\");"),
    ("impl Name {", "    pub fn new() -> Self {"),
    ("struct Name {", "    pub field: String,"),
    ("enum Name {", "    Value,"),
    ("trait Name {", "    fn name(&self);"),
    ("if condition {", "    run();"),
    ("} else {", "    run();"),
    ("for item in items {", "    println!(\"{item:?}\");"),
    ("while condition {", "    run();"),
    ("match value {", "    _ => {}"),
    ("function name() {", "  return value;"),
    ("if (condition) {", "  run();"),
    ("class Name {", "  constructor() {"),
    ("for (const item of items) {", "  console.log(item);"),
    ("def name():", "    return value"),
    ("for item in items:", "    print(item)"),
    ("if __name__ == \"__main__\":", "    main()"),
    ("func main() {", "    fmt.Println(\"Hello, world!\")"),
    ("if err != nil {", "    return err"),
    ("int main() {", "    return 0;"),
];

pub struct Predictor {
    learned: Vec<String>,
    transitions: Vec<(String, String)>,
    project: Vec<String>,
    path: Option<PathBuf>,
    transitions_path: Option<PathBuf>,
}

impl Predictor {
    pub fn load() -> Self {
        let path = cache_path("predictions.txt");
        let learned = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| line.trim_end().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let transitions_path = cache_path("transitions.txt");
        let transitions = transitions_path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| {
                text.lines()
                    .filter_map(|line| {
                        let value = serde_json::from_str::<Value>(line).ok()?;
                        let previous = value.get("p")?.as_str()?.to_string();
                        let next = value.get("n")?.as_str()?.to_string();
                        Some((previous, next))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut predictor = Self {
            learned,
            transitions,
            project: Vec::new(),
            path,
            transitions_path,
        };
        if predictor.learned.len() > MAX_LEARNED {
            let start = predictor.learned.len() - MAX_LEARNED;
            predictor.learned = predictor.learned.split_off(start);
        }
        if predictor.transitions.len() > MAX_TRANSITIONS {
            let start = predictor.transitions.len() - MAX_TRANSITIONS;
            predictor.transitions = predictor.transitions.split_off(start);
        }
        predictor
    }

    pub fn add_project_lines(&mut self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        self.project.extend(
            lines
                .into_iter()
                .filter(|line| !line.is_empty() && line.chars().count() <= MAX_LINE),
        );
        if self.project.len() > MAX_PROJECT_LINES {
            let start = self.project.len() - MAX_PROJECT_LINES;
            self.project.drain(0..start);
        }
    }

    pub fn suggest(
        &self,
        corpus: &[&str],
        line: &str,
        col: usize,
        previous: Option<&str>,
        indent: &str,
    ) -> Option<String> {
        let prefix: String = line.chars().take(col).collect();
        if prefix.trim().is_empty() {
            return self.suggest_next(corpus, previous, indent);
        }
        self.suggest_line(corpus, &prefix, previous, indent)
    }

    fn suggest_line(
        &self,
        corpus: &[&str],
        prefix: &str,
        previous: Option<&str>,
        indent: &str,
    ) -> Option<String> {
        let keys = suffix_keys(prefix);
        if keys.is_empty() {
            return None;
        }

        for key in &keys {
            let mut scores: BTreeMap<String, (u32, u32)> = BTreeMap::new();

            for (index, candidate) in corpus.iter().enumerate() {
                let previous_candidate = index
                    .checked_sub(1)
                    .and_then(|index| corpus.get(index))
                    .copied();
                self.score_candidate(
                    candidate,
                    previous_candidate,
                    key,
                    previous,
                    indent,
                    1,
                    &mut scores,
                );
            }
            for (index, candidate) in self.project.iter().enumerate() {
                let previous_candidate = index
                    .checked_sub(1)
                    .and_then(|index| self.project.get(index))
                    .map(String::as_str);
                self.score_candidate(
                    candidate,
                    previous_candidate,
                    key,
                    previous,
                    indent,
                    1,
                    &mut scores,
                );
            }
            for candidate in &self.learned {
                self.score_candidate(candidate, None, key, previous, indent, 4, &mut scores);
            }

            if let Some(best) = pick_best(scores) {
                return Some(best);
            }

            let mut defaults: BTreeMap<String, (u32, u32)> = BTreeMap::new();
            for candidate in DEFAULT_LINES {
                self.score_candidate(
                    candidate,
                    None,
                    key,
                    previous,
                    indent,
                    DEFAULT_WEIGHT,
                    &mut defaults,
                );
                if leading_whitespace(candidate).is_empty() && !indent.is_empty() {
                    let rebased = format!("{indent}{candidate}");
                    self.score_candidate(
                        &rebased,
                        None,
                        key,
                        previous,
                        indent,
                        DEFAULT_WEIGHT,
                        &mut defaults,
                    );
                }
            }
            if let Some(best) = pick_default(defaults) {
                return Some(best);
            }
        }

        None
    }

    fn suggest_next(
        &self,
        corpus: &[&str],
        previous: Option<&str>,
        indent: &str,
    ) -> Option<String> {
        let previous = previous?;
        let key = tail_key(previous, MAX_KEY)?;
        let mut scores: BTreeMap<String, (u32, u32)> = BTreeMap::new();

        for (known, next) in &self.transitions {
            if known == &key || eq_ignore_case(known, &key) {
                add_next(&mut scores, next, indent, 4);
            } else if ends_with_ignore_case(known, &key) {
                add_next(&mut scores, next, indent, 2);
            }
        }
        for pair in corpus.windows(2) {
            if tail_key(pair[0], MAX_KEY)
                .is_some_and(|candidate_key| eq_ignore_case(&candidate_key, &key))
            {
                add_next(&mut scores, pair[1], indent, 1);
            }
        }
        for pair in self.project.windows(2) {
            if tail_key(&pair[0], MAX_KEY)
                .is_some_and(|candidate_key| eq_ignore_case(&candidate_key, &key))
            {
                add_next(&mut scores, &pair[1], indent, 1);
            }
        }

        if let Some(best) = pick_best(scores) {
            return Some(best);
        }

        let mut defaults: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for (known, next) in DEFAULT_TRANSITIONS {
            if *known == key || eq_ignore_case(known, &key) {
                add_next(&mut defaults, next, indent, DEFAULT_WEIGHT);
            } else if ends_with_ignore_case(known, &key) {
                add_next(&mut defaults, next, indent, 2);
            }
        }
        pick_default(defaults)
    }

    #[allow(clippy::too_many_arguments)]
    fn score_candidate(
        &self,
        candidate: &str,
        previous_candidate: Option<&str>,
        key: &str,
        previous: Option<&str>,
        indent: &str,
        weight: u32,
        scores: &mut BTreeMap<String, (u32, u32)>,
    ) {
        let Some((_, end)) = rfind_ignore_case(candidate, key) else {
            return;
        };
        let tail = candidate[end..].trim_end();
        if tail.is_empty() || tail.chars().count() > MAX_TAIL || tail.chars().any(char::is_control)
        {
            return;
        }

        let mut score = weight * 2 + key.chars().count() as u32;
        let candidate_indent = leading_whitespace(candidate);
        if candidate_indent == indent {
            score += 4;
        } else if !candidate_indent.is_empty() && candidate_indent.starts_with(indent) {
            score += 1;
        }
        if let (Some(previous), Some(previous_candidate)) = (previous, previous_candidate) {
            if let Some(previous_key) = tail_key(previous, 12) {
                if ends_with_ignore_case(previous_candidate.trim_end(), &previous_key) {
                    score += 4;
                }
            }
        }

        scores
            .entry(tail.to_string())
            .and_modify(|(total, count)| {
                *total += score;
                *count += 1;
            })
            .or_insert((score, 1));
    }

    pub fn learn(&mut self, line: &str) {
        let line = line.trim_end();
        if line.trim().is_empty() || line.chars().count() > MAX_LINE {
            return;
        }

        self.learned.retain(|known| known != line);
        self.learned.push(line.to_string());
        if self.learned.len() > MAX_LEARNED {
            self.learned.remove(0);
        }

        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }

    pub fn learn_lines(&mut self, lines: &[String]) {
        for pair in lines.windows(2) {
            self.learn_transition(&pair[0], &pair[1]);
        }
    }

    pub fn learn_transition(&mut self, previous: &str, next: &str) {
        let Some(key) = tail_key(previous, MAX_KEY) else {
            return;
        };
        let next = next.trim_end();
        if next.trim().is_empty() || next.chars().count() > MAX_LINE {
            return;
        }
        if !next.starts_with(leading_whitespace(previous))
            && !leading_whitespace(previous).is_empty()
        {
            return;
        }

        if let Some(index) = self
            .transitions
            .iter()
            .position(|(known, line)| known == &key && line == next)
        {
            let entry = self.transitions.remove(index);
            self.transitions.push(entry);
        } else {
            self.transitions.push((key.clone(), next.to_string()));
            if self.transitions.len() > MAX_TRANSITIONS {
                self.transitions.remove(0);
            }
        }

        let Some(path) = &self.transitions_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", json!({ "p": key, "n": next }));
        }
    }
}

fn rfind_ignore_case(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() {
        return None;
    }
    let haystack_chars: Vec<(usize, char)> = haystack.char_indices().collect();
    if haystack_chars.len() < needle.len() {
        return None;
    }

    let mut found = None;
    for start in 0..=haystack_chars.len() - needle.len() {
        let matches = haystack_chars[start..start + needle.len()]
            .iter()
            .zip(&needle)
            .all(|((_, candidate), expected)| char_eq_ignore_case(*candidate, *expected));
        if matches {
            let end = haystack_chars
                .get(start + needle.len())
                .map(|(index, _)| *index)
                .unwrap_or(haystack.len());
            found = Some((haystack_chars[start].0, end));
        }
    }
    found
}

fn char_eq_ignore_case(left: char, right: char) -> bool {
    left == right || left.to_lowercase().eq(right.to_lowercase())
}

fn eq_ignore_case(left: &str, right: &str) -> bool {
    left.chars()
        .flat_map(char::to_lowercase)
        .eq(right.chars().flat_map(char::to_lowercase))
}

fn ends_with_ignore_case(haystack: &str, needle: &str) -> bool {
    let haystack_len = haystack.chars().count();
    let needle_len = needle.chars().count();
    if needle_len > haystack_len {
        return false;
    }
    if needle_len == haystack_len {
        return eq_ignore_case(haystack, needle);
    }
    let tail: String = haystack.chars().skip(haystack_len - needle_len).collect();
    eq_ignore_case(&tail, needle)
}

fn suffix_keys(prefix: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for length in [16usize, 12, 8, 6, 4] {
        let key: String = prefix
            .chars()
            .rev()
            .take(length)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if key.trim().chars().count() < 3 || keys.contains(&key) {
            continue;
        }
        keys.push(key);
    }
    keys
}

fn tail_key(line: &str, length: usize) -> Option<String> {
    let line = line.trim_end();
    if line.chars().count() < 3 {
        return None;
    }
    let key: String = line
        .chars()
        .rev()
        .take(length)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (key.chars().count() >= 3).then_some(key)
}

fn add_next(scores: &mut BTreeMap<String, (u32, u32)>, next: &str, indent: &str, weight: u32) {
    let next = next.trim_end();
    if next.is_empty() || next.chars().count() > MAX_LINE || next.chars().any(char::is_control) {
        return;
    }
    let next_indent = leading_whitespace(next);
    if !next_indent.starts_with(indent) {
        return;
    }
    let tail: String = next.chars().skip(indent.chars().count()).collect();
    if tail.trim().is_empty() {
        return;
    }

    let mut score = weight * 3;
    if next_indent == indent {
        score += 4;
    } else if next_indent.starts_with(indent) {
        score += 2;
    }
    scores
        .entry(tail)
        .and_modify(|(total, count)| {
            *total += score;
            *count += 1;
        })
        .or_insert((score, 1));
}

fn pick_best(scores: BTreeMap<String, (u32, u32)>) -> Option<String> {
    pick(scores, true)
}

fn pick_default(scores: BTreeMap<String, (u32, u32)>) -> Option<String> {
    pick(scores, false)
}

fn pick(scores: BTreeMap<String, (u32, u32)>, strict: bool) -> Option<String> {
    let mut best: Option<(u32, u32, String)> = None;
    for (tail, (score, count)) in scores {
        if score < MIN_SCORE || (strict && count < 2 && score < 12) {
            continue;
        }
        let replace = match &best {
            None => true,
            Some((best_score, best_count, best_tail)) => {
                score > *best_score
                    || (score == *best_score && count > *best_count)
                    || (score == *best_score
                        && count == *best_count
                        && tail.len() < best_tail.len())
            }
        };
        if replace {
            best = Some((score, count, tail));
        }
    }
    best.map(|(_, _, tail)| tail)
}

fn leading_whitespace(line: &str) -> &str {
    let end = line
        .char_indices()
        .take_while(|(_, ch)| *ch == ' ' || *ch == '\t')
        .map(|(index, ch)| index + ch.len_utf8())
        .last()
        .unwrap_or(0);
    &line[..end]
}

fn cache_path(name: &str) -> Option<PathBuf> {
    if let Some(base) = env::var_os("XDG_CACHE_HOME") {
        return Some(PathBuf::from(base).join("smol").join(name));
    }
    env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache").join("smol").join(name))
}

pub fn scan_project(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    collect_files(root, 0, &mut files);
    files.sort();

    let mut lines = Vec::new();
    let mut bytes = 0usize;
    for path in files.into_iter().take(300) {
        if bytes > 4 * 1024 * 1024 || lines.len() >= 20_000 {
            break;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > 256 * 1024 {
            continue;
        }
        let Ok(data) = fs::read(&path) else {
            continue;
        };
        if data.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(data) else {
            continue;
        };
        bytes += text.len();
        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if lines.len() >= 20_000 {
                break;
            }
            if !line.is_empty() && line.chars().count() <= MAX_LINE {
                lines.push(line.to_string());
            }
        }
    }
    lines
}

fn collect_files(root: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if IGNORED.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files(&path, depth + 1, files);
        } else if file_type.is_file() {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn predictor() -> Predictor {
        Predictor {
            learned: Vec::new(),
            transitions: Vec::new(),
            project: Vec::new(),
            path: None,
            transitions_path: None,
        }
    }

    #[test]
    fn predicts_repeated_suffix() {
        let predictor = predictor();
        let corpus = [
            "let value = compute();",
            "let value = compute();",
            "let other = 1;",
        ];
        let ghost = predictor.suggest(&corpus, "let val", 7, None, "");
        assert_eq!(ghost.as_deref(), Some("ue = compute();"));
    }

    #[test]
    fn learned_line_predicts_once() {
        let mut predictor = predictor();
        predictor.learned = vec!["    println!(\"done\");".to_string()];
        let ghost = predictor.suggest(&["fn main() {"], "    print", 9, None, "    ");
        assert_eq!(ghost.as_deref(), Some("ln!(\"done\");"));
    }

    #[test]
    fn project_lines_are_used() {
        let mut predictor = predictor();
        predictor.add_project_lines(vec![
            "    pub fn files(&self) -> Vec<String> {".to_string(),
            "    pub fn files(&self) -> Vec<String> {".to_string(),
        ]);
        let prefix = format!("    pub fn {}", "fil");
        let ghost = predictor.suggest(&[], &prefix, prefix.chars().count(), None, "    ");
        assert_eq!(ghost.as_deref(), Some("es(&self) -> Vec<String> {"));
    }

    #[test]
    fn predicts_next_line_from_transition() {
        let mut predictor = predictor();
        predictor.learn_transition("    if ready {", "        run();");
        let previous = "    if ready {";
        let ghost = predictor.suggest(&[], "    ", 4, Some(previous), "    ");
        assert_eq!(ghost.as_deref(), Some("    run();"));
    }

    #[test]
    fn predicts_next_line_from_project_pairs() {
        let mut predictor = predictor();
        predictor.add_project_lines(vec![
            "    if ready {".to_string(),
            "        run();".to_string(),
            "    if ready {".to_string(),
            "        run();".to_string(),
        ]);
        let ghost = predictor.suggest(&[], "    ", 4, Some("    if ready {"), "    ");
        assert_eq!(ghost.as_deref(), Some("    run();"));
    }

    #[test]
    fn previous_line_context_breaks_ties() {
        let predictor = predictor();
        let corpus = [
            "    let x = load();",
            "        let x = load();",
            "    let x = load();",
        ];
        let ghost = predictor.suggest(&corpus, "    let x", 9, Some("    let"), "    ");
        assert_eq!(ghost.as_deref(), Some(" = load();"));
    }

    #[test]
    fn project_scan_predicts_real_code() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut predictor = predictor();
        predictor.add_project_lines(scan_project(root));
        let prefix = format!("    pub fn {}", "track_label_f");
        let ghost = predictor.suggest(&[], &prefix, prefix.chars().count(), None, "    ");
        assert_eq!(
            ghost.as_deref(),
            Some("or(&self, path: &Path) -> Option<String> {")
        );
    }

    #[test]
    fn defaults_predict_common_words_without_history() {
        let predictor = predictor();
        let ghost = predictor.suggest(&[], "consol", 6, None, "");
        assert_eq!(ghost.as_deref(), Some("e.log(value);"));
    }

    #[test]
    fn defaults_predict_syntax_at_indent() {
        let predictor = predictor();
        let ghost = predictor.suggest(&[], "    printl", 10, None, "    ");
        assert_eq!(ghost.as_deref(), Some("n!(\"{value:?}\");"));
    }

    #[test]
    fn defaults_predict_next_line_without_history() {
        let predictor = predictor();
        let ghost = predictor.suggest(&[], "    ", 4, Some("fn main() {"), "    ");
        assert_eq!(ghost.as_deref(), Some("println!(\"Hello, world!\");"));
    }

    #[test]
    fn learned_lines_beat_defaults() {
        let mut predictor = predictor();
        predictor.learned = vec!["    println!(\"custom\");".to_string()];
        let ghost = predictor.suggest(&[], "    printl", 10, None, "    ");
        assert_eq!(ghost.as_deref(), Some("n!(\"custom\");"));
    }

    #[test]
    fn project_lines_beat_defaults() {
        let mut predictor = predictor();
        predictor.add_project_lines(vec!["    println!(\"project\");".to_string()]);
        let ghost = predictor.suggest(&[], "    printl", 10, None, "    ");
        assert_eq!(ghost.as_deref(), Some("n!(\"project\");"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let predictor = predictor();
        let corpus = ["Welcome to the project"];
        let ghost = predictor.suggest(&corpus, "welcome", 7, None, "");
        assert_eq!(ghost.as_deref(), Some(" to the project"));
    }

    #[test]
    fn default_matching_is_case_insensitive() {
        let predictor = predictor();
        let ghost = predictor.suggest(&[], "CONSOL", 6, None, "");
        assert_eq!(ghost.as_deref(), Some("e.log(value);"));
    }

    #[test]
    fn transition_matching_is_case_insensitive() {
        let mut predictor = predictor();
        predictor.transitions = vec![("fn main() {".to_string(), "        run();".to_string())];
        let ghost = predictor.suggest(&[], "    ", 4, Some("FN MAIN() {"), "    ");
        assert_eq!(ghost.as_deref(), Some("    run();"));
    }

    #[test]
    fn project_scan_skips_build_output() {
        let root = std::env::temp_dir().join(format!(
            "smol-scan-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("target")).expect("create target");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write source");
        fs::write(root.join("target/junk.rs"), "junk\n").expect("write junk");

        let lines = scan_project(&root);
        assert!(lines.iter().any(|line| line == "fn main() {}"));
        assert!(!lines.iter().any(|line| line == "junk"));
        let _ = fs::remove_dir_all(root);
    }
}
