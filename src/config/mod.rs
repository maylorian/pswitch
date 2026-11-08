//! Configuration: `config.yaml` (groups, projects, services) merged with
//! `~/.ssh/config` (hosts) into a single [`Tree`]. Pure apart from reading files.

pub mod ssh;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::paths::expand_tilde;
use ssh::{SshHost, push_unique};

pub const UNGROUPED: &str = "Ungrouped";

// ---------------------------------------------------------------- raw YAML --

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RawConfig {
    pub groups: Vec<RawGroup>,
    pub ssh: SshSettings,
    pub services: ServiceSettings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawGroup {
    pub name: String,
    #[serde(default)]
    pub entries: Vec<RawEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawEntry {
    pub project: Option<String>,
    pub service: Option<String>,
    pub ssh: Option<String>,
    pub dir: Option<String>,
    pub cmd: Option<String>,
    #[serde(default)]
    pub windows: Vec<RawWindow>,
    /// Override the tmux session name.
    pub session: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub own_session: Option<bool>,
    pub keep_on_exit: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawWindow {
    pub name: String,
    pub cmd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SshSettings {
    /// Hosts that no group mentions go to "Ungrouped".
    pub import_untagged: bool,
    pub session_prefix: String,
    /// Path of the ssh config to read.
    pub config: String,
    /// Default for hosts that don't set `keep_on_exit`.
    pub keep_on_exit: bool,
}

impl Default for SshSettings {
    fn default() -> Self {
        SshSettings {
            import_untagged: true,
            session_prefix: "ssh-".into(),
            config: "~/.ssh/config".into(),
            keep_on_exit: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServiceSettings {
    pub session_prefix: String,
}

impl Default for ServiceSettings {
    fn default() -> Self {
        ServiceSettings {
            session_prefix: "db-".into(),
        }
    }
}

pub fn parse_yaml(text: &str) -> Result<RawConfig> {
    if text.trim().is_empty() {
        return Ok(RawConfig::default());
    }
    Ok(serde_yaml_ng::from_str(text)?)
}

// ------------------------------------------------------------- merged tree --

#[derive(Debug, Clone, PartialEq)]
pub struct WindowSpec {
    pub name: String,
    pub cmd: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Project {
        dir: PathBuf,
        windows: Vec<WindowSpec>,
    },
    Service {
        dir: PathBuf,
        cmd: Option<String>,
    },
    Ssh {
        alias: String,
        hostname: Option<String>,
        user: Option<String>,
        own_session: bool,
        keep_on_exit: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// Groups in config order; the first one decides the tmux session.
    pub groups: Vec<String>,
    pub tags: Vec<String>,
    pub session: Option<String>,
}

impl Entry {
    /// Text the fuzzy filter matches against: name, host and tags.
    pub fn search_text(&self) -> String {
        let mut s = self.name.clone();
        if let Kind::Ssh {
            hostname: Some(h), ..
        } = &self.kind
        {
            s.push(' ');
            s.push_str(h);
        }
        for t in &self.tags {
            s.push(' ');
            s.push_str(t);
        }
        s
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.kind, Kind::Ssh { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub entries: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub ssh_prefix: String,
    pub service_prefix: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ssh_prefix: SshSettings::default().session_prefix,
            service_prefix: ServiceSettings::default().session_prefix,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tree {
    pub groups: Vec<Group>,
    pub entries: Vec<Entry>,
    pub settings: Settings,
}

/// Load `config.yaml` (optional) and the ssh config it points to.
pub fn load(config_path: &Path) -> Result<Tree> {
    let raw = match fs::read_to_string(config_path) {
        Ok(text) => {
            parse_yaml(&text).with_context(|| format!("parsing {}", config_path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RawConfig::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", config_path.display())),
    };
    let hosts = ssh::load(&expand_tilde(&raw.ssh.config));
    build(raw, &hosts)
}

#[derive(Hash, PartialEq, Eq)]
enum Key {
    Project(String),
    Service(String),
    Ssh(String),
}

#[derive(Default)]
struct Builder {
    entries: Vec<Entry>,
    index: HashMap<Key, usize>,
    groups: Vec<Group>,
}

impl Builder {
    fn group(&mut self, name: &str) -> usize {
        match self.groups.iter().position(|g| g.name == name) {
            Some(i) => i,
            None => {
                self.groups.push(Group {
                    name: name.to_string(),
                    entries: Vec::new(),
                });
                self.groups.len() - 1
            }
        }
    }

    fn add(&mut self, group: &str, idx: usize, tags: &[String]) {
        let g = self.group(group);
        if !self.groups[g].entries.contains(&idx) {
            self.groups[g].entries.push(idx);
        }
        let e = &mut self.entries[idx];
        push_unique(&mut e.groups, &[group.to_string()]);
        push_unique(&mut e.tags, tags);
    }

    fn get_or_insert(&mut self, key: Key, make: impl FnOnce() -> Entry) -> usize {
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        self.entries.push(make());
        self.index.insert(key, self.entries.len() - 1);
        self.entries.len() - 1
    }

    fn ssh(&mut self, alias: &str, host: Option<&SshHost>, defaults: &SshSettings) -> usize {
        self.get_or_insert(Key::Ssh(alias.to_string()), || {
            let ann = host.map(|h| h.ann.clone()).unwrap_or_default();
            Entry {
                name: alias.to_string(),
                kind: Kind::Ssh {
                    alias: alias.to_string(),
                    hostname: host.and_then(|h| h.hostname.clone()),
                    user: host.and_then(|h| h.user.clone()),
                    own_session: ann.own_session.unwrap_or(false),
                    keep_on_exit: ann.keep_on_exit.unwrap_or(defaults.keep_on_exit),
                },
                groups: Vec::new(),
                tags: ann.tags,
                session: None,
            }
        })
    }
}

/// Merge YAML and ssh hosts into one tree. Pure.
///
/// Group order: YAML groups, then groups only named in `# hop:` comments,
/// then "Ungrouped". An entry's first group (YAML before comments) sets its
/// tmux session.
pub fn build(raw: RawConfig, hosts: &[SshHost]) -> Result<Tree> {
    let by_alias: HashMap<&str, &SshHost> = hosts.iter().map(|h| (h.alias.as_str(), h)).collect();
    let mut b = Builder::default();

    for g in &raw.groups {
        if g.name.trim().is_empty() {
            bail!("a group has an empty name");
        }
        b.group(&g.name);
        for (i, e) in g.entries.iter().enumerate() {
            let at = || format!("group {:?}, entry {}", g.name, i + 1);
            let kinds = [&e.project, &e.service, &e.ssh]
                .iter()
                .filter(|x| x.is_some())
                .count();
            if kinds != 1 {
                bail!(
                    "{}: expected exactly one of `project`, `service` or `ssh`",
                    at()
                );
            }
            if let Some(name) = &e.project {
                let mut seen = Vec::new();
                for w in &e.windows {
                    if seen.contains(&w.name) {
                        bail!("{}: duplicate window name {:?}", at(), w.name);
                    }
                    seen.push(w.name.clone());
                }
                let idx = b.get_or_insert(Key::Project(name.clone()), || Entry {
                    name: name.clone(),
                    kind: Kind::Project {
                        dir: expand_tilde(e.dir.as_deref().unwrap_or("~")),
                        windows: e
                            .windows
                            .iter()
                            .map(|w| WindowSpec {
                                name: w.name.clone(),
                                cmd: w.cmd.clone(),
                            })
                            .collect(),
                    },
                    groups: Vec::new(),
                    tags: Vec::new(),
                    session: e.session.clone(),
                });
                b.add(&g.name, idx, &e.tags);
            } else if let Some(name) = &e.service {
                let idx = b.get_or_insert(Key::Service(name.clone()), || Entry {
                    name: name.clone(),
                    kind: Kind::Service {
                        dir: expand_tilde(e.dir.as_deref().unwrap_or("~")),
                        cmd: e.cmd.clone(),
                    },
                    groups: Vec::new(),
                    tags: Vec::new(),
                    session: e.session.clone(),
                });
                b.add(&g.name, idx, &e.tags);
            } else if let Some(pat) = &e.ssh {
                let aliases: Vec<String> = if pat.contains(['*', '?', '[']) {
                    let p = glob::Pattern::new(pat)
                        .with_context(|| format!("{}: bad glob {:?}", at(), pat))?;
                    hosts
                        .iter()
                        .filter(|h| p.matches(&h.alias))
                        .map(|h| h.alias.clone())
                        .collect()
                } else {
                    vec![pat.clone()]
                };
                for alias in aliases {
                    let idx = b.ssh(&alias, by_alias.get(alias.as_str()).copied(), &raw.ssh);
                    let entry = &mut b.entries[idx];
                    if let Kind::Ssh {
                        own_session,
                        keep_on_exit,
                        ..
                    } = &mut entry.kind
                    {
                        if let Some(v) = e.own_session {
                            *own_session = v;
                        }
                        if let Some(v) = e.keep_on_exit {
                            *keep_on_exit = v;
                        }
                    }
                    if e.session.is_some() {
                        entry.session = e.session.clone();
                    }
                    b.add(&g.name, idx, &e.tags);
                }
            }
        }
    }

    for h in hosts {
        for g in &h.ann.groups {
            let idx = b.ssh(&h.alias, Some(h), &raw.ssh);
            b.add(g, idx, &h.ann.tags);
        }
    }

    if raw.ssh.import_untagged {
        for h in hosts {
            if !b.index.contains_key(&Key::Ssh(h.alias.clone())) {
                let idx = b.ssh(&h.alias, Some(h), &raw.ssh);
                b.add(UNGROUPED, idx, &[]);
            }
        }
    }

    Ok(Tree {
        groups: b.groups,
        entries: b.entries,
        settings: Settings {
            ssh_prefix: raw.ssh.session_prefix,
            service_prefix: raw.services.session_prefix,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_YAML: &str = r#"
groups:
  - name: Work
    entries:
      - project: portal-scraper
        dir: ~/code/portal-scraper
        windows:
          - { name: editor, cmd: nvim }
          - { name: shell }
          - { name: logs, cmd: "tail -f logs/app.log" }
      - service: postgres
        dir: ~
        cmd: "docker compose -f ~/infra/db.yml up"
      - ssh: prod-db1        # alias from ~/.ssh/config
      - ssh: "staging-*"     # glob: every matching alias
  - name: Personal
    entries:
      - ssh: homelab

ssh:
  import_untagged: true      # hosts with no group go to "Ungrouped"
  session_prefix: ssh-
"#;

    const SPEC_SSH: &str = "
# hop: group=Work tags=db,prod
Host prod-db1
    HostName 10.0.0.12
    User andreas

Host staging-api staging-web
Host homelab
Host random-box
# hop: group=Infra,Personal
Host bastion
";

    fn tree() -> Tree {
        let hosts = ssh::parse_str(SPEC_SSH, Path::new("/"));
        build(parse_yaml(SPEC_YAML).unwrap(), &hosts).unwrap()
    }

    fn names(t: &Tree, group: &str) -> Vec<String> {
        let g = t.groups.iter().find(|g| g.name == group).unwrap();
        g.entries
            .iter()
            .map(|&i| t.entries[i].name.clone())
            .collect()
    }

    #[test]
    fn spec_example_merges() {
        let t = tree();
        let groups: Vec<_> = t.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(groups, ["Work", "Personal", "Infra", "Ungrouped"]);
        assert_eq!(
            names(&t, "Work"),
            [
                "portal-scraper",
                "postgres",
                "prod-db1",
                "staging-api",
                "staging-web"
            ]
        );
        assert_eq!(names(&t, "Personal"), ["homelab", "bastion"]);
        assert_eq!(names(&t, "Infra"), ["bastion"]);
        assert_eq!(names(&t, "Ungrouped"), ["random-box"]);
    }

    #[test]
    fn entries_keep_host_info_and_tags() {
        let t = tree();
        let prod = t.entries.iter().find(|e| e.name == "prod-db1").unwrap();
        assert_eq!(prod.groups, ["Work"]);
        assert_eq!(prod.tags, ["db", "prod"]);
        assert!(matches!(&prod.kind, Kind::Ssh { hostname: Some(h), .. } if h == "10.0.0.12"));
        // Comment groups come after YAML groups for first-group purposes.
        let bastion = t.entries.iter().find(|e| e.name == "bastion").unwrap();
        assert_eq!(bastion.groups, ["Infra", "Personal"]);
    }

    #[test]
    fn project_windows_and_dir() {
        let t = tree();
        let p = t
            .entries
            .iter()
            .find(|e| e.name == "portal-scraper")
            .unwrap();
        let Kind::Project { dir, windows } = &p.kind else {
            panic!()
        };
        assert!(dir.ends_with("code/portal-scraper"));
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[2].cmd.as_deref(), Some("tail -f logs/app.log"));
    }

    #[test]
    fn untagged_import_can_be_disabled() {
        let hosts = ssh::parse_str("Host a\nHost b\n", Path::new("/"));
        let raw = parse_yaml("ssh: { import_untagged: false }").unwrap();
        let t = build(raw, &hosts).unwrap();
        assert!(t.entries.is_empty());
    }

    #[test]
    fn yaml_overrides_annotation_options() {
        let hosts = ssh::parse_str("# hop: keep_on_exit=true\nHost a\n", Path::new("/"));
        let raw = parse_yaml(
            "groups: [{name: G, entries: [{ssh: a, keep_on_exit: false, own_session: true}]}]",
        )
        .unwrap();
        let t = build(raw, &hosts).unwrap();
        assert!(matches!(
            t.entries[0].kind,
            Kind::Ssh {
                keep_on_exit: false,
                own_session: true,
                ..
            }
        ));
    }

    #[test]
    fn unknown_ssh_alias_still_listed() {
        let raw = parse_yaml("groups: [{name: G, entries: [{ssh: not-in-config}]}]").unwrap();
        let t = build(raw, &[]).unwrap();
        assert_eq!(t.entries[0].name, "not-in-config");
    }

    #[test]
    fn rejects_ambiguous_entries() {
        let raw = parse_yaml("groups: [{name: G, entries: [{ssh: a, project: b}]}]").unwrap();
        assert!(build(raw, &[]).is_err());
        assert!(parse_yaml("groups: [{name: G, entries: [{sssh: a}]}]").is_err());
    }

    #[test]
    fn empty_yaml_is_default() {
        let raw = parse_yaml("  \n").unwrap();
        assert!(raw.ssh.import_untagged);
        assert_eq!(raw.ssh.session_prefix, "ssh-");
    }

    #[test]
    fn same_entry_in_two_groups() {
        let raw = parse_yaml(
            "groups:\n - {name: A, entries: [{project: p, dir: /tmp}]}\n - {name: B, entries: [{project: p, tags: [x]}]}",
        )
        .unwrap();
        let t = build(raw, &[]).unwrap();
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].groups, ["A", "B"]);
        assert_eq!(t.entries[0].tags, ["x"]);
    }
}
