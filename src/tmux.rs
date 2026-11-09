//! Thin wrapper over the `tmux` binary. The only module with side effects.
//!
//! Every lookup uses exact-match targets (`=name`, `=session:=window`) so
//! `db` never matches `db-postgres`.

use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::target::{Run, Target, WinPlan};

pub const HOME_SESSION: &str = "home";

/// Running state of the tmux server, captured once per refresh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Session name -> number of windows.
    pub sessions: HashMap<String, usize>,
    pub windows: HashSet<(String, String)>,
}

impl Snapshot {
    pub fn parse(list_windows: &str) -> Self {
        let mut s = Snapshot::default();
        for line in list_windows.lines() {
            // Session names can't contain ':', so the first one splits.
            let Some((sess, win)) = line.split_once(':') else {
                continue;
            };
            *s.sessions.entry(sess.to_string()).or_default() += 1;
            s.windows.insert((sess.to_string(), win.to_string()));
        }
        s
    }

    pub fn running(&self, t: &Target) -> bool {
        match &t.window {
            Some(w) => self.windows.contains(&(t.session.clone(), w.clone())),
            None => self.sessions.contains_key(&t.session),
        }
    }

    pub fn window_count(&self, session: &str) -> Option<usize> {
        self.sessions.get(session).copied()
    }
}

/// True when running inside a tmux client (including a popup).
pub fn inside_tmux() -> bool {
    env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

fn exact_session(s: &str) -> String {
    format!("={s}")
}

fn exact_window(s: &str, w: &str) -> String {
    format!("={s}:={w}")
}

#[derive(Debug, Clone, Default)]
pub struct Tmux {
    /// `-L socket`; `None` uses the default server.
    pub socket: Option<String>,
}

impl Tmux {
    pub fn new(socket: Option<String>) -> Self {
        Tmux { socket }
    }

    /// Honours `$HOP_TMUX_SOCKET` (handy for testing against a scratch server).
    pub fn from_env() -> Self {
        Tmux::new(env::var("HOP_TMUX_SOCKET").ok().filter(|s| !s.is_empty()))
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(s) = &self.socket {
            c.arg("-L").arg(s);
        }
        c
    }

    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<String> {
        let out = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context("running tmux (is it installed?)")?;
        if !out.status.success() {
            let cmd = args
                .iter()
                .map(|a| a.as_ref().to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            bail!(
                "tmux {cmd}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn ok<S: AsRef<OsStr>>(&self, args: &[S]) -> bool {
        self.command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn has_session(&self, session: &str) -> bool {
        self.ok(&["has-session", "-t", &exact_session(session)])
    }

    pub fn has_window(&self, session: &str, window: &str) -> bool {
        self.run(&[
            "list-windows",
            "-t",
            &exact_session(session),
            "-F",
            "#{window_name}",
        ])
        .is_ok_and(|out| out.lines().any(|l| l == window))
    }

    /// Current sessions and windows; empty if no server is running.
    pub fn snapshot(&self) -> Snapshot {
        self.run(&["list-windows", "-a", "-F", "#{session_name}:#{window_name}"])
            .map(|out| Snapshot::parse(&out))
            .unwrap_or_default()
    }

    /// Create the target if it is missing. Returns whether anything was created.
    pub fn ensure(&self, t: &Target) -> Result<bool> {
        match &t.window {
            None => {
                if self.has_session(&t.session) {
                    return Ok(false);
                }
                let (first, rest) = t
                    .windows
                    .split_first()
                    .ok_or_else(|| anyhow!("{}: no windows", t.session))?;
                self.create(t, first, true)?;
                for w in rest {
                    self.create(t, w, false)?;
                }
                // Land on the first window, like a fresh session would.
                let _ = self.run(&[
                    "select-window",
                    "-t",
                    &exact_window(&t.session, &first.name),
                ]);
                Ok(true)
            }
            Some(w) => {
                let plan = t
                    .windows
                    .iter()
                    .find(|p| &p.name == w)
                    .ok_or_else(|| anyhow!("{}: no plan", t.label()))?;
                if self.has_session(&t.session) {
                    if self.has_window(&t.session, w) {
                        return Ok(false);
                    }
                    self.create(t, plan, false)?;
                } else {
                    self.create(t, plan, true)?;
                }
                Ok(true)
            }
        }
    }

    fn create(&self, t: &Target, w: &WinPlan, new_session: bool) -> Result<()> {
        let dir = t.dir.to_string_lossy().into_owned();
        let mut args: Vec<String> = if new_session {
            [
                "new-session",
                "-d",
                "-s",
                &t.session,
                "-n",
                &w.name,
                "-c",
                &dir,
            ]
            .map(String::from)
            .to_vec()
        } else {
            let at = format!("{}:", exact_session(&t.session));
            ["new-window", "-d", "-t", &at, "-n", &w.name, "-c", &dir]
                .map(String::from)
                .to_vec()
        };
        if let Run::Exec(argv) = &w.run {
            args.push("--".into());
            args.extend(argv.iter().cloned());
        }
        self.run(&args)?;
        let win = exact_window(&t.session, &w.name);
        // Keep our names stable so lookups keep working. The window may
        // already be gone if its command exited at once, so ignore errors.
        let _ = self.run(&["set-option", "-w", "-t", &win, "automatic-rename", "off"]);
        let _ = self.run(&["set-option", "-w", "-t", &win, "allow-rename", "off"]);
        if let Run::Type(cmd) = &w.run {
            self.run(&["send-keys", "-t", &win, "-l", cmd])?;
            self.run(&["send-keys", "-t", &win, "Enter"])?;
        }
        Ok(())
    }

    fn select(&self, t: &Target) -> Result<()> {
        if let Some(w) = &t.window {
            self.run(&["select-window", "-t", &exact_window(&t.session, w)])
                .with_context(|| {
                    format!("{} is gone (did the command exit right away?)", t.label())
                })?;
        }
        Ok(())
    }

    /// From inside tmux: move this client to the target.
    pub fn switch(&self, t: &Target) -> Result<()> {
        self.select(t)?;
        self.run(&["switch-client", "-t", &exact_session(&t.session)])?;
        Ok(())
    }

    /// From outside tmux: replace this process with `tmux attach`.
    pub fn attach(&self, t: &Target) -> anyhow::Error {
        if let Err(e) = self.select(t) {
            return e;
        }
        let err = self
            .command()
            .args(["attach-session", "-t", &exact_session(&t.session)])
            .exec();
        anyhow!(err).context("exec tmux attach-session")
    }

    /// Start the server with a `home` session (or reuse it) and attach.
    pub fn attach_home(&self) -> anyhow::Error {
        let err = self
            .command()
            .args(["new-session", "-A", "-s", HOME_SESSION])
            .exec();
        anyhow!(err).context("exec tmux new-session")
    }

    pub fn kill(&self, t: &Target) -> Result<()> {
        match &t.window {
            Some(w) => self.run(&["kill-window", "-t", &exact_window(&t.session, w)])?,
            None => self.run(&["kill-session", "-t", &exact_session(&t.session)])?,
        };
        Ok(())
    }

    /// Split the current pane and run `argv` in the new one.
    /// `vertical` means side by side (vim's `:vsplit`, tmux's `-h`).
    pub fn split(&self, argv: &[String], vertical: bool) -> Result<()> {
        let mut args: Vec<String> = vec![
            "split-window".into(),
            if vertical { "-h" } else { "-v" }.into(),
            "--".into(),
        ];
        args.extend(argv.iter().cloned());
        self.run(&args)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn snapshot_parse() {
        let s = Snapshot::parse("ssh-work:prod-db1\nssh-work:a:b\nhome:zsh\n");
        assert_eq!(s.window_count("ssh-work"), Some(2));
        assert!(s.windows.contains(&("ssh-work".into(), "a:b".into())));
        let t = Target {
            session: "ssh-work".into(),
            window: Some("prod-db1".into()),
            dir: PathBuf::new(),
            windows: vec![],
        };
        assert!(s.running(&t));
        let t = Target {
            session: "ssh".into(),
            window: None,
            dir: PathBuf::new(),
            windows: vec![],
        };
        assert!(!s.running(&t));
    }

    /// Runs against an isolated server (`tmux -L hop-test-...`), never yours.
    struct Server(Tmux);

    impl Server {
        fn new(name: &str) -> Option<Self> {
            if Command::new("tmux").arg("-V").output().is_err() {
                eprintln!("tmux not installed; skipping");
                return None;
            }
            let t = Tmux::new(Some(format!("hop-test-{}-{name}", std::process::id())));
            // -f /dev/null: ignore the user's tmux.conf.
            t.run(&["-f", "/dev/null", "start-server"]).ok();
            Some(Server(t))
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.run(&["kill-server"]);
        }
    }

    fn ssh_like(session: &str, window: &str) -> Target {
        Target {
            session: session.into(),
            window: Some(window.into()),
            dir: std::env::temp_dir(),
            windows: vec![WinPlan {
                name: window.into(),
                run: Run::Exec(vec!["sleep".into(), "300".into()]),
            }],
        }
    }

    #[test]
    fn ensure_window_is_idempotent() {
        let Some(s) = Server::new("win") else { return };
        let a = ssh_like("ssh-work", "prod-db1");
        let b = ssh_like("ssh-work", "staging-api");
        assert!(s.0.ensure(&a).unwrap());
        assert!(!s.0.ensure(&a).unwrap());
        assert!(s.0.ensure(&b).unwrap());
        assert!(!s.0.ensure(&b).unwrap());
        let snap = s.0.snapshot();
        assert_eq!(snap.window_count("ssh-work"), Some(2));
        assert!(snap.running(&a) && snap.running(&b));
        s.0.kill(&a).unwrap();
        let snap = s.0.snapshot();
        assert!(!snap.running(&a) && snap.running(&b));
    }

    #[test]
    fn exact_match_lookups() {
        let Some(s) = Server::new("exact") else {
            return;
        };
        let long = Target {
            window: None,
            ..ssh_like("db-postgres", "postgres")
        };
        s.0.ensure(&long).unwrap();
        assert!(s.0.has_session("db-postgres"));
        assert!(!s.0.has_session("db"));
        let short = Target {
            window: None,
            ..ssh_like("db", "x")
        };
        assert!(
            s.0.ensure(&short).unwrap(),
            "`db` must not match `db-postgres`"
        );
        assert!(!s.0.has_window("db-postgres", "post"));
    }

    #[test]
    fn ensure_project_session_creates_all_windows() {
        let Some(s) = Server::new("proj") else { return };
        let t = Target {
            session: "portal".into(),
            window: None,
            dir: std::env::temp_dir(),
            windows: vec![
                WinPlan {
                    name: "editor".into(),
                    run: Run::Type("echo editor".into()),
                },
                WinPlan {
                    name: "shell".into(),
                    run: Run::Shell,
                },
                WinPlan {
                    name: "logs".into(),
                    run: Run::Exec(vec!["sleep".into(), "300".into()]),
                },
            ],
        };
        assert!(s.0.ensure(&t).unwrap());
        assert!(!s.0.ensure(&t).unwrap());
        let names =
            s.0.run(&["list-windows", "-t", "=portal", "-F", "#{window_name}"])
                .unwrap();
        assert_eq!(
            names.lines().collect::<Vec<_>>(),
            ["editor", "shell", "logs"]
        );
        let active =
            s.0.run(&[
                "list-windows",
                "-t",
                "=portal",
                "-F",
                "#{window_active}#{window_name}",
            ])
            .unwrap();
        assert!(active.lines().any(|l| l == "1editor"), "{active}");
        let snap = s.0.snapshot();
        assert_eq!(snap.window_count("portal"), Some(3));
        s.0.kill(&t).unwrap();
        assert!(!s.0.has_session("portal"));
    }

    #[test]
    fn snapshot_without_server_is_empty() {
        let t = Tmux::new(Some(format!("hop-test-none-{}", std::process::id())));
        assert_eq!(t.snapshot(), Snapshot::default());
    }
}
