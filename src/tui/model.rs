//! Picker state and key handling. Pure: keys go in, [`Action`]s come out.

use std::collections::HashMap;

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use crate::config::{Kind, Tree, UNGROUPED};
use crate::target::{self, Target};
use crate::tmux::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Backspace,
    Home,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    /// Attach-or-create these entries; the last one gets focus.
    Open(Vec<usize>),
    Split {
        entry: usize,
        vertical: bool,
    },
    Kill(usize),
    Edit,
    Reload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Confirm(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Group(usize),
    /// (group index, entry index)
    Entry(usize, usize),
}

pub struct Model {
    pub tree: Tree,
    pub targets: Vec<Target>,
    pub snap: Snapshot,
    pub expanded: HashMap<String, bool>,
    pub cursor: usize,
    pub query: String,
    pub mode: Mode,
    pub marked: Vec<usize>,
    pub message: Option<String>,
    /// Per entry: does it match `query`? `None` when there is no query.
    matches: Option<Vec<bool>>,
}

impl Model {
    pub fn new(tree: Tree, snap: Snapshot, expanded: HashMap<String, bool>) -> Self {
        let mut m = Model {
            targets: Vec::new(),
            tree: Tree::default(),
            snap,
            expanded,
            cursor: 0,
            query: String::new(),
            mode: Mode::Normal,
            marked: Vec::new(),
            message: None,
            matches: None,
        };
        m.set_tree(tree);
        m
    }

    /// Replace the tree (on reload), keeping expand state and the query.
    pub fn set_tree(&mut self, tree: Tree) {
        self.targets = tree
            .entries
            .iter()
            .map(|e| target::for_entry(e, &tree.settings))
            .collect();
        self.tree = tree;
        self.marked.clear();
        if self.mode != Mode::Filter {
            self.mode = Mode::Normal;
        }
        self.refilter();
        self.clamp();
    }

    pub fn running(&self, entry: usize) -> bool {
        self.snap.running(&self.targets[entry])
    }

    pub fn is_expanded(&self, group: &str) -> bool {
        if self.matches.is_some() {
            return true;
        }
        self.expanded
            .get(group)
            .copied()
            .unwrap_or(group != UNGROUPED)
    }

    fn visible(&self, entry: usize) -> bool {
        self.matches.as_ref().is_none_or(|m| m[entry])
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (gi, g) in self.tree.groups.iter().enumerate() {
            let entries: Vec<usize> = g
                .entries
                .iter()
                .copied()
                .filter(|&e| self.visible(e))
                .collect();
            if self.matches.is_some() && entries.is_empty() {
                continue;
            }
            rows.push(Row::Group(gi));
            if self.is_expanded(&g.name) {
                rows.extend(entries.into_iter().map(|e| Row::Entry(gi, e)));
            }
        }
        rows
    }

    pub fn current(&self) -> Option<Row> {
        self.rows().get(self.cursor).copied()
    }

    fn clamp(&mut self) {
        let n = self.rows().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn move_by(&mut self, delta: isize) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.matches = None;
            return;
        }
        let matcher = SkimMatcherV2::default();
        self.matches = Some(
            self.tree
                .entries
                .iter()
                .map(|e| matcher.fuzzy_match(&e.search_text(), &self.query).is_some())
                .collect(),
        );
        // Put the cursor on the first match.
        self.cursor = self
            .rows()
            .iter()
            .position(|r| matches!(r, Row::Entry(..)))
            .unwrap_or(0);
    }

    fn set_expanded(&mut self, group: usize, value: bool) {
        let name = self.tree.groups[group].name.clone();
        self.expanded.insert(name, value);
    }

    fn toggle_mark(&mut self, e: usize) {
        match self.marked.iter().position(|&m| m == e) {
            Some(i) => {
                self.marked.remove(i);
            }
            None => self.marked.push(e),
        }
    }

    fn activate(&mut self) -> Action {
        match self.current() {
            _ if !self.marked.is_empty() => Action::Open(self.marked.clone()),
            Some(Row::Entry(_, e)) => Action::Open(vec![e]),
            Some(Row::Group(g)) => {
                let name = self.tree.groups[g].name.clone();
                if self.matches.is_none() {
                    let v = !self.is_expanded(&name);
                    self.set_expanded(g, v);
                }
                Action::None
            }
            None => Action::None,
        }
    }

    pub fn handle(&mut self, key: Key) -> Action {
        self.message = None;
        if key == Key::Ctrl('c') {
            return Action::Quit;
        }
        match self.mode {
            Mode::Confirm(e) => {
                self.mode = Mode::Normal;
                if matches!(key, Key::Char('y' | 'Y')) {
                    return Action::Kill(e);
                }
                self.message = Some("Kill cancelled".into());
                Action::None
            }
            Mode::Filter => self.handle_filter(key),
            Mode::Normal => self.handle_normal(key),
        }
    }

    fn handle_filter(&mut self, key: Key) -> Action {
        match key {
            Key::Esc => {
                self.query.clear();
                self.mode = Mode::Normal;
                self.refilter();
                self.clamp();
            }
            Key::Enter => {
                self.mode = Mode::Normal;
                return self.activate();
            }
            Key::Backspace => {
                if self.query.pop().is_none() {
                    self.mode = Mode::Normal;
                }
                self.refilter();
                self.clamp();
            }
            Key::Ctrl('u') => {
                self.query.clear();
                self.refilter();
                self.clamp();
            }
            Key::Up | Key::Ctrl('p' | 'k') => self.move_by(-1),
            Key::Down | Key::Ctrl('n' | 'j') => self.move_by(1),
            Key::Tab => {
                if let Some(Row::Entry(_, e)) = self.current() {
                    self.toggle_mark(e);
                    self.move_by(1);
                }
            }
            Key::Char(c) => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        Action::None
    }

    fn handle_normal(&mut self, key: Key) -> Action {
        let row = self.current();
        match key {
            Key::Char('q') => return Action::Quit,
            Key::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.refilter();
                    self.clamp();
                } else if !self.marked.is_empty() {
                    self.marked.clear();
                } else {
                    return Action::Quit;
                }
            }
            Key::Char('j') | Key::Down | Key::Ctrl('n') => self.move_by(1),
            Key::Char('k') | Key::Up | Key::Ctrl('p') => self.move_by(-1),
            Key::Char('g') | Key::Home => self.cursor = 0,
            Key::Char('G') | Key::End => self.cursor = self.rows().len().saturating_sub(1),
            Key::Char('h') | Key::Left => {
                if self.matches.is_some() {
                    return Action::None;
                }
                let g = match row {
                    Some(Row::Group(g) | Row::Entry(g, _)) => g,
                    None => return Action::None,
                };
                self.set_expanded(g, false);
                self.cursor = self
                    .rows()
                    .iter()
                    .position(|r| *r == Row::Group(g))
                    .unwrap_or(0);
            }
            Key::Char('l') | Key::Right => {
                if let Some(Row::Group(g)) = row {
                    self.set_expanded(g, true);
                }
            }
            Key::Enter => return self.activate(),
            Key::Char(' ') | Key::Tab => match row {
                Some(Row::Entry(_, e)) => {
                    self.toggle_mark(e);
                    self.move_by(1);
                }
                Some(Row::Group(_)) => return self.activate(),
                None => {}
            },
            Key::Char('/') => self.mode = Mode::Filter,
            Key::Char(c @ ('v' | 's')) => {
                if let Some(Row::Entry(_, e)) = row {
                    if self.tree.entries[e].is_ssh() {
                        return Action::Split {
                            entry: e,
                            vertical: c == 'v',
                        };
                    }
                    self.message = Some("Splits only work for SSH hosts".into());
                }
            }
            Key::Char('x') => {
                if let Some(Row::Entry(_, e)) = row {
                    if self.running(e) {
                        self.mode = Mode::Confirm(e);
                    } else {
                        self.message =
                            Some(format!("{} is not running", self.tree.entries[e].name));
                    }
                }
            }
            Key::Char('e') => return Action::Edit,
            Key::Char('r') => return Action::Reload,
            _ => {}
        }
        Action::None
    }

    /// Short description shown next to an entry.
    pub fn info(&self, entry: usize) -> String {
        let e = &self.tree.entries[entry];
        match &e.kind {
            Kind::Project { .. } => {
                let t = &self.targets[entry];
                let n = self
                    .snap
                    .window_count(&t.session)
                    .unwrap_or(t.windows.len());
                format!("session · {n} window{}", if n == 1 { "" } else { "s" })
            }
            Kind::Service { .. } => "service".into(),
            Kind::Ssh { hostname, user, .. } => match (user, hostname) {
                (Some(u), Some(h)) => format!("ssh · {u}@{h}"),
                (None, Some(h)) => format!("ssh · {h}"),
                _ => "ssh".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{build, parse_yaml, ssh};
    use std::path::Path;

    fn model() -> Model {
        let hosts = ssh::parse_str(
            "# hop: group=Work tags=db\nHost prod-db1\n HostName 10.0.0.12\nHost staging-api\nHost homelab\nHost loose\n",
            Path::new("/"),
        );
        let raw = parse_yaml(
            "groups:\n - name: Work\n   entries: [{project: portal, dir: /tmp}, {ssh: 'staging-*'}]\n - name: Personal\n   entries: [{ssh: homelab}]",
        )
        .unwrap();
        let tree = build(raw, &hosts).unwrap();
        let snap = Snapshot::parse("ssh-work:prod-db1\n");
        Model::new(tree, snap, HashMap::new())
    }

    fn names(m: &Model) -> Vec<String> {
        m.rows()
            .iter()
            .map(|r| match *r {
                Row::Group(g) => format!("[{}]", m.tree.groups[g].name),
                Row::Entry(_, e) => m.tree.entries[e].name.clone(),
            })
            .collect()
    }

    fn entry(m: &Model, name: &str) -> usize {
        m.tree.entries.iter().position(|e| e.name == name).unwrap()
    }

    #[test]
    fn default_tree_collapses_ungrouped() {
        let m = model();
        assert_eq!(
            names(&m),
            [
                "[Work]",
                "portal",
                "staging-api",
                "prod-db1",
                "[Personal]",
                "homelab",
                "[Ungrouped]"
            ]
        );
    }

    #[test]
    fn collapse_from_entry_moves_to_group() {
        let mut m = model();
        m.handle(Key::Char('j'));
        m.handle(Key::Char('j'));
        assert_eq!(m.handle(Key::Char('h')), Action::None);
        assert_eq!(m.cursor, 0);
        assert_eq!(
            names(&m),
            ["[Work]", "[Personal]", "homelab", "[Ungrouped]"]
        );
        m.handle(Key::Char('l'));
        assert_eq!(m.rows().len(), 7);
    }

    #[test]
    fn enter_opens_or_toggles() {
        let mut m = model();
        assert_eq!(m.handle(Key::Enter), Action::None); // group row toggles
        assert_eq!(names(&m)[..2], ["[Work]", "[Personal]"]);
        m.handle(Key::Char('G'));
        m.handle(Key::Enter); // expand Ungrouped
        m.handle(Key::Char('j'));
        assert_eq!(m.handle(Key::Enter), Action::Open(vec![entry(&m, "loose")]));
    }

    #[test]
    fn filter_expands_matching_groups_only() {
        let mut m = model();
        m.handle(Key::Char('/'));
        for c in "loo".chars() {
            m.handle(Key::Char(c));
        }
        assert_eq!(names(&m), ["[Ungrouped]", "loose"]);
        assert_eq!(m.cursor, 1);
        assert_eq!(m.handle(Key::Enter), Action::Open(vec![entry(&m, "loose")]));
        // Hostnames and tags are searchable too.
        m.handle(Key::Esc);
        m.handle(Key::Char('/'));
        for c in "10.0".chars() {
            m.handle(Key::Char(c));
        }
        assert_eq!(names(&m), ["[Work]", "prod-db1"]);
        m.handle(Key::Esc);
        assert_eq!(m.query, "");
        assert_eq!(m.rows().len(), 7);
    }

    #[test]
    fn marks_open_together() {
        let mut m = model();
        m.handle(Key::Char('j'));
        m.handle(Key::Char(' ')); // portal, cursor -> staging-api
        m.handle(Key::Char(' ')); // staging-api
        let expected = vec![entry(&m, "portal"), entry(&m, "staging-api")];
        assert_eq!(m.handle(Key::Enter), Action::Open(expected));
    }

    #[test]
    fn kill_requires_running_and_confirmation() {
        let mut m = model();
        m.cursor = 1; // portal: not running
        m.handle(Key::Char('x'));
        assert!(m.message.as_deref().unwrap().contains("not running"));
        m.cursor = 3; // prod-db1: running
        m.handle(Key::Char('x'));
        assert_eq!(m.mode, Mode::Confirm(entry(&m, "prod-db1")));
        assert_eq!(m.handle(Key::Char('n')), Action::None);
        m.handle(Key::Char('x'));
        assert_eq!(
            m.handle(Key::Char('y')),
            Action::Kill(entry(&m, "prod-db1"))
        );
    }

    #[test]
    fn split_only_for_ssh() {
        let mut m = model();
        m.cursor = 1;
        assert_eq!(m.handle(Key::Char('v')), Action::None);
        m.cursor = 2;
        assert_eq!(
            m.handle(Key::Char('s')),
            Action::Split {
                entry: entry(&m, "staging-api"),
                vertical: false
            }
        );
    }

    #[test]
    fn info_and_running() {
        let m = model();
        assert!(m.running(entry(&m, "prod-db1")));
        assert!(!m.running(entry(&m, "homelab")));
        assert_eq!(m.info(entry(&m, "prod-db1")), "ssh · 10.0.0.12");
        assert_eq!(m.info(entry(&m, "portal")), "session · 1 window");
    }
}
