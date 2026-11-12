use std::fs;
use std::path::{Path, PathBuf};

const IGNORED: &[&str] = &[".git", "node_modules", "target", ".freebuff", ".smol"];

pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub loaded: bool,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub visible: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
}

impl FileTree {
    pub fn new(root: &Path) -> Self {
        let root = root.to_path_buf();
        let name = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| root.display().to_string());
        let mut tree = Self {
            root,
            nodes: vec![Node {
                path: PathBuf::new(),
                name,
                is_dir: true,
                depth: 0,
                expanded: true,
                loaded: false,
                children: Vec::new(),
                parent: None,
            }],
            visible: Vec::new(),
            selected: 0,
            scroll: 0,
        };
        tree.nodes[0].path = tree.root.clone();
        tree.load_children(0);
        tree.rebuild_visible();
        tree
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.nodes.get(*index))
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected_node().map(|node| node.path.clone())
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, self.visible.len().saturating_sub(1) as isize) as usize;
    }

    pub fn select_row(&mut self, row: usize) {
        let index = self.scroll + row;
        if index < self.visible.len() {
            self.selected = index;
        }
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.visible.len().saturating_sub(1);
    }

    pub fn activate(&mut self) -> Option<PathBuf> {
        let index = *self.visible.get(self.selected)?;
        if self.nodes[index].is_dir {
            self.toggle(index);
            None
        } else {
            Some(self.nodes[index].path.clone())
        }
    }

    pub fn expand_selected(&mut self) {
        if let Some(&index) = self.visible.get(self.selected) {
            if self.nodes[index].is_dir && !self.nodes[index].expanded {
                self.toggle(index);
            }
        }
    }

    pub fn collapse_selected(&mut self) {
        if let Some(&index) = self.visible.get(self.selected) {
            if self.nodes[index].is_dir && self.nodes[index].expanded {
                self.toggle(index);
            } else if let Some(parent) = self.nodes[index].parent {
                if let Some(pos) = self.visible.iter().position(|found| *found == parent) {
                    self.selected = pos;
                }
            }
        }
    }

    pub fn refresh(&mut self) {
        let expanded: Vec<PathBuf> = self
            .nodes
            .iter()
            .filter(|node| node.is_dir && node.expanded && node.depth > 0)
            .map(|node| node.path.clone())
            .collect();
        let selected = self.selected_path();

        self.nodes.truncate(1);
        self.nodes[0].loaded = false;
        self.nodes[0].children.clear();
        self.nodes[0].expanded = true;
        self.load_children(0);
        for path in &expanded {
            self.expand_only(path);
        }
        self.rebuild_visible();

        if let Some(selected) = selected {
            if let Some(position) = self
                .visible
                .iter()
                .position(|index| self.nodes[*index].path == selected)
            {
                self.selected = position;
            }
        }
    }

    pub fn reveal(&mut self, path: &Path) {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };

        let mut current = 0usize;
        let mut found = true;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                continue;
            };
            let name = name.to_string_lossy();
            let children = self.nodes[current].children.clone();
            let Some(child) = children
                .into_iter()
                .find(|index| self.nodes[*index].name == name)
            else {
                found = false;
                break;
            };
            if self.nodes[child].is_dir {
                if !self.nodes[child].loaded {
                    self.load_children(child);
                }
                self.nodes[child].expanded = true;
            }
            current = child;
        }

        if !found {
            return;
        }

        self.rebuild_visible();
        if let Some(position) = self.visible.iter().position(|index| *index == current) {
            self.selected = position;
        }
    }

    fn expand_only(&mut self, path: &Path) {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        let mut current = 0usize;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                continue;
            };
            let name = name.to_string_lossy();
            let children = self.nodes[current].children.clone();
            let Some(child) = children
                .into_iter()
                .find(|index| self.nodes[*index].name == name)
            else {
                return;
            };
            if self.nodes[child].is_dir {
                if !self.nodes[child].loaded {
                    self.load_children(child);
                }
                self.nodes[child].expanded = true;
            }
            current = child;
        }
    }

    fn toggle(&mut self, index: usize) {
        if self.nodes[index].is_dir {
            if !self.nodes[index].loaded {
                self.load_children(index);
            }
            self.nodes[index].expanded = !self.nodes[index].expanded;
            self.rebuild_visible();
        }
    }

    fn load_children(&mut self, index: usize) {
        if self.nodes[index].loaded {
            return;
        }
        self.nodes[index].loaded = true;

        let path = self.nodes[index].path.clone();
        let depth = self.nodes[index].depth + 1;
        let Ok(entries) = fs::read_dir(&path) else {
            return;
        };

        let mut children = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.is_empty() || IGNORED.contains(&name.as_str()) {
                continue;
            }
            let entry_path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let is_dir = file_type.is_dir() || (file_type.is_symlink() && entry_path.is_dir());
            if !is_dir && !file_type.is_file() {
                continue;
            }

            let child = self.nodes.len();
            self.nodes.push(Node {
                path: entry_path,
                name,
                is_dir,
                depth,
                expanded: false,
                loaded: false,
                children: Vec::new(),
                parent: Some(index),
            });
            children.push(child);
        }

        children.sort_by(|a, b| {
            let left = &self.nodes[*a];
            let right = &self.nodes[*b];
            right
                .is_dir
                .cmp(&left.is_dir)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        self.nodes[index].children = children;
    }

    fn rebuild_visible(&mut self) {
        self.visible.clear();
        let children = self.nodes[0].children.clone();
        for child in children {
            self.walk(child);
        }
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
    }

    fn walk(&mut self, index: usize) {
        self.visible.push(index);
        if self.nodes[index].is_dir && self.nodes[index].expanded {
            let children = self.nodes[index].children.clone();
            for child in children {
                self.walk(child);
            }
        }
    }
}
