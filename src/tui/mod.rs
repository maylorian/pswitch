//! The interactive picker: event loop wiring the model to tmux.

pub mod model;
mod view;

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::config::{self, Tree};
use crate::state::{self, State};
use crate::target::{self, Target};
use crate::tmux::{self, Tmux};
use model::{Action, Key, Model};

pub enum Outcome {
    /// Nothing to do (closed, or already switched inside tmux).
    Done,
    Cancelled,
    /// Outside tmux: attach to this once the terminal is restored.
    Attach(Target),
}

const STARTER_CONFIG: &str = include_str!("../../examples/config.yaml");

fn load_tree(cfg: &Path) -> (Tree, Option<String>) {
    match config::load(cfg) {
        Ok(t) => (t, None),
        Err(e) => (Tree::default(), Some(format!("config error: {e:#}"))),
    }
}

pub fn run(tmux: &Tmux, cfg: &Path, state_path: &Path) -> Result<Outcome> {
    let (tree, err) = load_tree(cfg);
    let mut m = Model::new(tree, tmux.snapshot(), state::load(state_path).expanded);
    m.message = err;

    let mut term = ratatui::init();
    let res = event_loop(&mut term, &mut m, tmux, cfg);
    ratatui::restore();

    let _ = state::save(
        state_path,
        &State {
            expanded: m.expanded.clone(),
        },
    );
    res
}

fn map_key(k: event::KeyEvent) -> Option<Key> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    Some(match k.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    })
}

fn event_loop(
    term: &mut DefaultTerminal,
    m: &mut Model,
    tmux: &Tmux,
    cfg: &Path,
) -> Result<Outcome> {
    let mut list = ListState::default();
    loop {
        term.draw(|f| view::draw(f, m, &mut list))?;
        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        let Some(key) = map_key(k) else { continue };
        match m.handle(key) {
            Action::None => {}
            Action::Quit => return Ok(Outcome::Cancelled),
            Action::Open(entries) => {
                let targets: Vec<Target> = entries.iter().map(|&e| m.targets[e].clone()).collect();
                match open(tmux, &targets) {
                    Ok(o) => return Ok(o),
                    Err(e) => {
                        m.message = Some(format!("{e:#}"));
                        m.snap = tmux.snapshot();
                    }
                }
            }
            Action::Split { entry, vertical } => {
                if !tmux::inside_tmux() {
                    m.message = Some("Splits need a tmux client; run hop inside tmux".into());
                    continue;
                }
                let target::Run::Exec(argv) = &m.targets[entry].windows[0].run else {
                    continue;
                };
                match tmux.split(argv, vertical) {
                    Ok(()) => return Ok(Outcome::Done),
                    Err(e) => m.message = Some(format!("{e:#}")),
                }
            }
            Action::Kill(e) => {
                let t = m.targets[e].clone();
                m.message = Some(match tmux.kill(&t) {
                    Ok(()) => format!("Killed {}", t.label()),
                    Err(err) => format!("{err:#}"),
                });
                m.snap = tmux.snapshot();
            }
            Action::Edit => {
                ratatui::restore();
                let res = edit(cfg);
                *term = ratatui::init();
                reload(m, tmux, cfg);
                if let Err(e) = res {
                    m.message = Some(format!("{e:#}"));
                }
            }
            Action::Reload => {
                reload(m, tmux, cfg);
                if m.message.is_none() {
                    m.message = Some("Reloaded".into());
                }
            }
        }
    }
}

fn reload(m: &mut Model, tmux: &Tmux, cfg: &Path) {
    let (tree, err) = load_tree(cfg);
    m.set_tree(tree);
    m.snap = tmux.snapshot();
    m.message = err;
}

/// Ensure every target exists, then focus the last one.
pub fn open(tmux: &Tmux, targets: &[Target]) -> Result<Outcome> {
    for t in targets {
        tmux.ensure(t)?;
    }
    let Some(last) = targets.last() else {
        return Ok(Outcome::Done);
    };
    if tmux::inside_tmux() {
        tmux.switch(last)?;
        Ok(Outcome::Done)
    } else {
        Ok(Outcome::Attach(last.clone()))
    }
}

fn edit(cfg: &Path) -> Result<()> {
    if !cfg.exists() {
        if let Some(dir) = cfg.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(cfg, STARTER_CONFIG).with_context(|| format!("writing {}", cfg.display()))?;
    }
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    // Let the shell split "$EDITOR" (e.g. "code -w").
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("hop-edit")
        .arg(cfg)
        .status()
        .context("starting editor")?;
    if !status.success() {
        anyhow::bail!("editor exited with {status}");
    }
    Ok(())
}
