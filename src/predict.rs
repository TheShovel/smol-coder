use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const MAX_LEARNED: usize = 3000;
const MAX_TAIL: usize = 120;

pub struct Predictor {
    learned: Vec<String>,
    path: Option<PathBuf>,
}

impl Predictor {
    pub fn load() -> Self {
        let path = cache_path();
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

        let mut predictor = Self { learned, path };
        if predictor.learned.len() > MAX_LEARNED {
            let start = predictor.learned.len() - MAX_LEARNED;
            predictor.learned = predictor.learned.split_off(start);
        }
        predictor
    }

    pub fn suggest<'a, I>(&self, corpus: I, line: &str, col: usize) -> Option<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let prefix: String = line.chars().take(col).collect();
        let key: String = prefix
            .chars()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        if key.trim().chars().count() < 3 {
            return None;
        }

        let mut counts: BTreeMap<String, u32> = BTreeMap::new();

        for candidate in corpus {
            add_candidate(&mut counts, candidate, &key, 1);
        }
        for candidate in &self.learned {
            add_candidate(&mut counts, candidate, &key, 3);
        }

        let mut best: Option<(u32, String)> = None;
        for (tail, count) in counts {
            if count < 2 {
                continue;
            }
            let replace = match &best {
                None => true,
                Some((best_count, best_tail)) => {
                    count > *best_count || (count == *best_count && tail.len() < best_tail.len())
                }
            };
            if replace {
                best = Some((count, tail));
            }
        }

        best.map(|(_, tail)| tail)
    }

    pub fn learn(&mut self, line: &str) {
        let line = line.trim_end();
        if line.trim().is_empty() || line.chars().count() > 400 {
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
}

fn add_candidate(counts: &mut BTreeMap<String, u32>, text: &str, key: &str, weight: u32) {
    let Some(pos) = text.rfind(key) else {
        return;
    };
    let tail = text[pos + key.len()..].trim_end();
    if tail.is_empty() || tail.chars().count() > MAX_TAIL {
        return;
    }
    if tail.chars().any(char::is_control) {
        return;
    }
    *counts.entry(tail.to_string()).or_insert(0) += weight;
}

fn cache_path() -> Option<PathBuf> {
    if let Some(base) = env::var_os("XDG_CACHE_HOME") {
        return Some(PathBuf::from(base).join("smol").join("predictions.txt"));
    }
    env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".cache")
            .join("smol")
            .join("predictions.txt")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicts_repeated_suffix() {
        let predictor = Predictor {
            learned: Vec::new(),
            path: None,
        };
        let corpus = [
            "let value = compute();",
            "let value = compute();",
            "let other = 1;",
        ];
        let ghost = predictor.suggest(corpus, "let val", 7);
        assert_eq!(ghost.as_deref(), Some("ue = compute();"));
    }

    #[test]
    fn learned_line_predicts_once() {
        let predictor = Predictor {
            learned: vec!["    println!(\"done\");".to_string()],
            path: None,
        };
        let ghost = predictor.suggest(["fn main() {"], "    print", 9);
        assert_eq!(ghost.as_deref(), Some("ln!(\"done\");"));
    }
}
