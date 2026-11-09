//! hop: open any project or server with one keypress and Enter.

mod config;
mod paths;
mod state;
mod target;
mod tmux;
mod tui;

use std::path::PathBuf;

use anyhow::{Result, bail};

use config::Tree;
use target::Target;
use tmux::Tmux;
use tui::Outcome;

const HELP: &str = "\
hop - tmux session & SSH launcher

USAGE:
    hop                 open the picker (use inside a tmux popup)
    hop <name>...       attach-or-create without the UI; the last name gets focus
    hop --list          print entries: name, kind, tmux target, running

OPTIONS:
    -c, --config <path> config file (default: $HOP_CONFIG or ~/.config/hop/config.yaml)
    -h, --help          show this help
    -V, --version       show version

tmux binding:
    bind Space display-popup -E -w 60% -h 70% hop
";

fn main() {
    if let Err(e) = real_main() {
        eprintln!("hop: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let mut cfg = paths::config_path();
    let mut list = false;
    let mut names = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("hop {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-l" | "--list" => list = true,
            "-c" | "--config" => match args.next() {
                Some(p) => cfg = PathBuf::from(p),
                None => bail!("{a} needs a path"),
            },
            "--" => names.extend(args.by_ref()),
            s if s.starts_with('-') && s.len() > 1 => bail!("unknown option {s} (try --help)"),
            _ => names.push(a),
        }
    }

    let tmux = Tmux::from_env();
    if list {
        return print_list(&tmux, &config::load(&cfg)?);
    }
    if !names.is_empty() {
        let tree = config::load(&cfg)?;
        let targets = names
            .iter()
            .map(|n| resolve(&tree, n))
            .collect::<Result<Vec<_>>>()?;
        return finish(&tmux, tui::open(&tmux, &targets)?);
    }
    finish(&tmux, tui::run(&tmux, &cfg, &paths::state_path())?)
}

fn finish(tmux: &Tmux, outcome: Outcome) -> Result<()> {
    match outcome {
        Outcome::Done => Ok(()),
        Outcome::Attach(t) => Err(tmux.attach(&t)),
        // Outside tmux, closing the picker still lands you in tmux.
        Outcome::Cancelled if !tmux::inside_tmux() => Err(tmux.attach_home()),
        Outcome::Cancelled => Ok(()),
    }
}

/// Find an entry by its name, then by its tmux session/window name.
fn resolve(tree: &Tree, name: &str) -> Result<Target> {
    let targets: Vec<Target> = tree
        .entries
        .iter()
        .map(|e| target::for_entry(e, &tree.settings))
        .collect();
    let by_name = tree.entries.iter().position(|e| e.name == name);
    let by_tmux = || {
        targets.iter().position(|t| match &t.window {
            Some(w) => w == name || t.label() == name,
            None => t.session == name,
        })
    };
    let by_case = || {
        tree.entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(name))
    };
    match by_name.or_else(by_tmux).or_else(by_case) {
        Some(i) => Ok(targets[i].clone()),
        None => bail!("no entry named {name:?} (see `hop --list`)"),
    }
}

fn print_list(tmux: &Tmux, tree: &Tree) -> Result<()> {
    let snap = tmux.snapshot();
    for e in &tree.entries {
        let t = target::for_entry(e, &tree.settings);
        let kind = match e.kind {
            config::Kind::Project { .. } => "project",
            config::Kind::Service { .. } => "service",
            config::Kind::Ssh { .. } => "ssh",
        };
        let state = if snap.running(&t) {
            "running"
        } else {
            "stopped"
        };
        println!("{}\t{}\t{}\t{}", e.name, kind, t.label(), state);
    }
    Ok(())
}
