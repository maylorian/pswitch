//! Read-only `~/.ssh/config` parser.
//!
//! Extracts concrete host aliases (wildcard patterns are skipped), their
//! `HostName`/`User`, follows `Include` directives, and picks up
//! `# hop: group=Work tags=db,prod` annotation comments placed above a
//! `Host` line.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::expand_tilde;

const MAX_INCLUDE_DEPTH: usize = 16;

/// Options set by a `# hop:` comment above a `Host` line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Annotation {
    pub groups: Vec<String>,
    pub tags: Vec<String>,
    pub keep_on_exit: Option<bool>,
    pub own_session: Option<bool>,
}

impl Annotation {
    fn merge(&mut self, other: &Annotation) {
        push_unique(&mut self.groups, &other.groups);
        push_unique(&mut self.tags, &other.tags);
        if other.keep_on_exit.is_some() {
            self.keep_on_exit = other.keep_on_exit;
        }
        if other.own_session.is_some() {
            self.own_session = other.own_session;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SshHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub ann: Annotation,
}

/// Parse an ssh config file. A missing file yields no hosts.
pub fn load(path: &Path) -> Vec<SshHost> {
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut p = Parser::new(base);
    p.file(path, 0);
    p.hosts
}

#[cfg(test)]
/// Parse ssh config text; relative `Include` paths resolve against `base`.
pub fn parse_str(text: &str, base: &Path) -> Vec<SshHost> {
    let mut p = Parser::new(base.to_path_buf());
    p.text(text, 0);
    p.hosts
}

struct Parser {
    base: PathBuf,
    hosts: Vec<SshHost>,
    index: HashMap<String, usize>,
}

impl Parser {
    fn new(base: PathBuf) -> Self {
        Parser {
            base,
            hosts: Vec::new(),
            index: HashMap::new(),
        }
    }

    fn file(&mut self, path: &Path, depth: usize) {
        if depth > MAX_INCLUDE_DEPTH {
            return;
        }
        if let Ok(text) = fs::read_to_string(path) {
            self.text(&text, depth);
        }
    }

    fn text(&mut self, text: &str, depth: usize) {
        // Host indices the current `Host` block applies to.
        let mut block: Vec<usize> = Vec::new();
        let mut pending: Option<Annotation> = None;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                if let Some(a) = comment.trim_start().strip_prefix("hop:") {
                    pending = Some(parse_annotation(a));
                }
                continue;
            }
            let (kw, args) = split_directive(line);
            match kw.to_ascii_lowercase().as_str() {
                "host" => {
                    let ann = pending.take().unwrap_or_default();
                    block.clear();
                    for alias in args.into_iter().filter(|a| !is_pattern(a)) {
                        let idx = match self.index.get(&alias) {
                            Some(&i) => i,
                            None => {
                                self.hosts.push(SshHost {
                                    alias: alias.clone(),
                                    ..Default::default()
                                });
                                self.index.insert(alias, self.hosts.len() - 1);
                                self.hosts.len() - 1
                            }
                        };
                        self.hosts[idx].ann.merge(&ann);
                        block.push(idx);
                    }
                }
                "match" => {
                    pending = None;
                    block.clear();
                }
                "include" => {
                    pending = None;
                    for arg in args {
                        for path in self.expand_include(&arg) {
                            self.file(&path, depth + 1);
                        }
                    }
                }
                "hostname" => {
                    pending = None;
                    // ssh uses the first value obtained for each option.
                    for &i in &block {
                        if self.hosts[i].hostname.is_none() {
                            self.hosts[i].hostname = args.first().cloned();
                        }
                    }
                }
                "user" => {
                    pending = None;
                    for &i in &block {
                        if self.hosts[i].user.is_none() {
                            self.hosts[i].user = args.first().cloned();
                        }
                    }
                }
                _ => pending = None,
            }
        }
    }

    fn expand_include(&self, arg: &str) -> Vec<PathBuf> {
        let mut path = expand_tilde(arg);
        if path.is_relative() {
            path = self.base.join(path);
        }
        let s = path.to_string_lossy();
        if !s.contains(['*', '?', '[']) {
            return vec![path];
        }
        let mut out: Vec<PathBuf> = glob::glob(&s)
            .map(|paths| paths.filter_map(Result::ok).collect())
            .unwrap_or_default();
        out.sort();
        out
    }
}

/// `Host` arguments with wildcards or negation are patterns, not aliases.
fn is_pattern(s: &str) -> bool {
    s.contains(['*', '?']) || s.starts_with('!')
}

/// Split `Keyword args` / `Keyword=args` into the keyword and its arguments.
fn split_directive(line: &str) -> (String, Vec<String>) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let kw = &line[..end];
    let mut rest = line[end..].trim_start();
    if let Some(r) = rest.strip_prefix('=') {
        rest = r.trim_start();
    }
    (kw.to_string(), tokenize(rest))
}

/// Whitespace split that honours double quotes.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has_token = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has_token = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

fn parse_annotation(s: &str) -> Annotation {
    let mut ann = Annotation::default();
    for tok in s.split_whitespace() {
        let Some((k, v)) = tok.split_once('=') else {
            continue;
        };
        let list = || {
            v.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        };
        match k.to_ascii_lowercase().as_str() {
            "group" | "groups" => push_unique(&mut ann.groups, &list()),
            "tag" | "tags" => push_unique(&mut ann.tags, &list()),
            "keep_on_exit" => ann.keep_on_exit = parse_bool(v),
            "own_session" => ann.own_session = parse_bool(v),
            _ => {}
        }
    }
    ann
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub(crate) fn push_unique(dst: &mut Vec<String>, src: &[String]) {
    for s in src {
        if !dst.contains(s) {
            dst.push(s.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(hosts: &[SshHost]) -> Vec<&str> {
        hosts.iter().map(|h| h.alias.as_str()).collect()
    }

    #[test]
    fn parses_hosts_hostnames_and_skips_wildcards() {
        let text = "\
Host *
    ServerAliveInterval 30

Host prod-db1
    HostName 10.0.0.12
    User andreas

Host staging-api staging-web *.internal
  hostname=staging.example.com

Host !bad
";
        let hosts = parse_str(text, Path::new("/nonexistent"));
        assert_eq!(aliases(&hosts), ["prod-db1", "staging-api", "staging-web"]);
        assert_eq!(hosts[0].hostname.as_deref(), Some("10.0.0.12"));
        assert_eq!(hosts[0].user.as_deref(), Some("andreas"));
        assert_eq!(hosts[2].hostname.as_deref(), Some("staging.example.com"));
    }

    #[test]
    fn first_hostname_wins() {
        let hosts = parse_str(
            "Host a\n HostName one\nHost a\n HostName two\n",
            Path::new("/"),
        );
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].hostname.as_deref(), Some("one"));
    }

    #[test]
    fn hop_annotation_applies_to_next_host_only() {
        let text = "\
# hop: group=Work tags=db,prod keep_on_exit=true
# some other comment
Host prod-db1
    HostName 10.0.0.12

Host homelab

# hop: group=Personal
User nobody
Host stray
";
        let hosts = parse_str(text, Path::new("/"));
        assert_eq!(hosts[0].ann.groups, ["Work"]);
        assert_eq!(hosts[0].ann.tags, ["db", "prod"]);
        assert_eq!(hosts[0].ann.keep_on_exit, Some(true));
        assert_eq!(hosts[1].ann, Annotation::default());
        // An intervening directive cancels the annotation.
        assert_eq!(hosts[2].ann, Annotation::default());
    }

    #[test]
    fn match_blocks_do_not_leak_hostname() {
        let hosts = parse_str("Host a\nMatch host foo\n HostName nope\n", Path::new("/"));
        assert_eq!(hosts[0].hostname, None);
    }

    #[test]
    fn follows_includes_relative_and_glob() {
        let dir = std::env::temp_dir().join(format!("hop-ssh-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config.d")).unwrap();
        fs::write(dir.join("config"), "Include config.d/*\nHost main\n").unwrap();
        fs::write(dir.join("config.d/b"), "Host from-b\n").unwrap();
        fs::write(
            dir.join("config.d/a"),
            "# hop: group=Infra\nHost from-a\n HostName 1.2.3.4\n",
        )
        .unwrap();
        let hosts = load(&dir.join("config"));
        assert_eq!(aliases(&hosts), ["from-a", "from-b", "main"]);
        assert_eq!(hosts[0].ann.groups, ["Infra"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn include_cycles_terminate() {
        let dir = std::env::temp_dir().join(format!("hop-ssh-cycle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config"), "Include config\nHost x\n").unwrap();
        let hosts = load(&dir.join("config"));
        assert_eq!(aliases(&hosts), ["x"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quoted_args() {
        assert_eq!(tokenize(r#"a "b c" d"#), ["a", "b c", "d"]);
        assert_eq!(split_directive("HostName=example.com").1, ["example.com"]);
    }
}
