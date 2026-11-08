//! Mapping from config entries to tmux sessions/windows. Pure.

use std::path::PathBuf;

use crate::config::{Entry, Kind, Settings};
use crate::paths::home;

/// What runs in a window when it is created.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    /// Just the user's shell.
    Shell,
    /// The user's shell, with this command typed into it (the window
    /// survives the command exiting).
    Type(String),
    /// Exec this argv as the window's process; the window closes when it exits.
    Exec(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WinPlan {
    pub name: String,
    pub run: Run,
}

/// Where an entry lives in tmux, and how to create it.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub session: String,
    /// `Some` when the entry is one window inside a shared session (SSH hosts).
    /// `None` when the entry owns the whole session.
    pub window: Option<String>,
    pub dir: PathBuf,
    /// Windows to create. With `window: Some(w)` this is exactly that window.
    pub windows: Vec<WinPlan>,
}

impl Target {
    /// `session` or `session:window`, for display.
    pub fn label(&self) -> String {
        match &self.window {
            Some(w) => format!("{}:{}", self.session, w),
            None => self.session.clone(),
        }
    }
}

/// Make a name tmux-safe: `.`, `:` and whitespace become `-`.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '.' || c == ':' || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// argv for the window process of an SSH host.
pub fn ssh_argv(alias: &str, keep_on_exit: bool) -> Vec<String> {
    if !keep_on_exit {
        return vec!["ssh".into(), alias.into()];
    }
    // Plain POSIX sh, so the user's login shell (fish, ...) doesn't matter.
    // `trap : INT`: a Ctrl-C that reaches ssh (e.g. while connecting) ends
    // ssh but not this loop; a handler, unlike `trap ''`, isn't inherited.
    let script = r#"trap : INT
while :; do
ssh "$1"
printf '\n[exited] r: reconnect, any other key: close '
s=$(stty -g); stty raw -echo; k=$(dd bs=1 count=1 2>/dev/null); stty "$s"
[ "$k" = r ] || break
printf '\n'
done"#;
    vec![
        "sh".into(),
        "-c".into(),
        script.into(),
        "hop-ssh".into(),
        alias.into(),
    ]
}

pub fn for_entry(e: &Entry, s: &Settings) -> Target {
    match &e.kind {
        Kind::Project { dir, windows } => {
            let mut plans: Vec<WinPlan> = windows
                .iter()
                .map(|w| WinPlan {
                    name: sanitize(&w.name),
                    run: match &w.cmd {
                        Some(c) if !c.trim().is_empty() => Run::Type(c.clone()),
                        _ => Run::Shell,
                    },
                })
                .collect();
            if plans.is_empty() {
                plans.push(WinPlan {
                    name: "shell".into(),
                    run: Run::Shell,
                });
            }
            Target {
                session: e.session.clone().unwrap_or_else(|| sanitize(&e.name)),
                window: None,
                dir: dir.clone(),
                windows: plans,
            }
        }
        Kind::Service { dir, cmd } => Target {
            session: e
                .session
                .clone()
                .unwrap_or_else(|| format!("{}{}", s.service_prefix, sanitize(&e.name))),
            window: None,
            dir: dir.clone(),
            windows: vec![WinPlan {
                name: sanitize(&e.name),
                run: cmd
                    .clone()
                    .filter(|c| !c.trim().is_empty())
                    .map(Run::Type)
                    .unwrap_or(Run::Shell),
            }],
        },
        Kind::Ssh {
            alias,
            own_session,
            keep_on_exit,
            ..
        } => {
            let plan = WinPlan {
                name: sanitize(alias),
                run: Run::Exec(ssh_argv(alias, *keep_on_exit)),
            };
            if *own_session {
                Target {
                    session: e
                        .session
                        .clone()
                        .unwrap_or_else(|| format!("{}{}", s.ssh_prefix, sanitize(alias))),
                    window: None,
                    dir: home(),
                    windows: vec![plan],
                }
            } else {
                let group = e
                    .groups
                    .first()
                    .map(String::as_str)
                    .unwrap_or(crate::config::UNGROUPED);
                Target {
                    session: e.session.clone().unwrap_or_else(|| {
                        format!("{}{}", s.ssh_prefix, sanitize(&group.to_lowercase()))
                    }),
                    window: Some(plan.name.clone()),
                    dir: home(),
                    windows: vec![plan],
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WindowSpec;

    fn ssh(alias: &str, groups: &[&str], own: bool) -> Entry {
        Entry {
            name: alias.into(),
            kind: Kind::Ssh {
                alias: alias.into(),
                hostname: None,
                user: None,
                own_session: own,
                keep_on_exit: false,
            },
            groups: groups.iter().map(|s| s.to_string()).collect(),
            tags: vec![],
            session: None,
        }
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("web.example.com:22"), "web-example-com-22");
        assert_eq!(sanitize("My Group"), "My-Group");
    }

    #[test]
    fn ssh_host_is_window_in_group_session() {
        let t = for_entry(
            &ssh("prod-db1", &["Work", "Infra"], false),
            &Settings::default(),
        );
        assert_eq!(t.session, "ssh-work");
        assert_eq!(t.window.as_deref(), Some("prod-db1"));
        assert_eq!(
            t.windows[0].run,
            Run::Exec(vec!["ssh".into(), "prod-db1".into()])
        );
        assert_eq!(t.label(), "ssh-work:prod-db1");
    }

    #[test]
    fn ssh_own_session() {
        let t = for_entry(
            &ssh("db.example.com", &["Work"], true),
            &Settings::default(),
        );
        assert_eq!(t.session, "ssh-db-example-com");
        assert_eq!(t.window, None);
    }

    #[test]
    fn service_session_name() {
        let e = Entry {
            name: "postgres".into(),
            kind: Kind::Service {
                dir: "/".into(),
                cmd: Some("docker compose up".into()),
            },
            groups: vec!["Work".into()],
            tags: vec![],
            session: None,
        };
        let t = for_entry(&e, &Settings::default());
        assert_eq!(t.session, "db-postgres");
        assert_eq!(t.windows[0].run, Run::Type("docker compose up".into()));
    }

    #[test]
    fn project_default_window() {
        let mut e = Entry {
            name: "my.proj".into(),
            kind: Kind::Project {
                dir: "/".into(),
                windows: vec![],
            },
            groups: vec![],
            tags: vec![],
            session: None,
        };
        let t = for_entry(&e, &Settings::default());
        assert_eq!(t.session, "my-proj");
        assert_eq!(
            t.windows,
            vec![WinPlan {
                name: "shell".into(),
                run: Run::Shell
            }]
        );
        e.kind = Kind::Project {
            dir: "/".into(),
            windows: vec![WindowSpec {
                name: "ed".into(),
                cmd: Some("nvim".into()),
            }],
        };
        assert_eq!(
            for_entry(&e, &Settings::default()).windows[0].run,
            Run::Type("nvim".into())
        );
    }
}
