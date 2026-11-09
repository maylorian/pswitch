//! Filesystem locations: home, XDG config/state dirs, `~` expansion.

use std::env;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` or `~/` to the home directory.
pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        home()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(s)
    }
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

/// `$HOP_CONFIG`, else `$XDG_CONFIG_HOME/hop/config.yaml`.
pub fn config_path() -> PathBuf {
    match env::var_os("HOP_CONFIG") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => xdg("XDG_CONFIG_HOME", ".config").join("hop/config.yaml"),
    }
}

/// `$HOP_STATE`, else `$XDG_STATE_HOME/hop/state.json`.
pub fn state_path() -> PathBuf {
    match env::var_os("HOP_STATE") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => xdg("XDG_STATE_HOME", ".local/state").join("hop/state.json"),
    }
}
