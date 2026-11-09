# hop

A tmux popup that opens any project, service or SSH host with one keypress and
Enter. One terminal, one tmux server, no duplicate sessions: if the target
exists `hop` switches to it, otherwise it creates it and then switches.

```
▾ Work
    ● portal-scraper  session · 3 windows
    ○ postgres        service
    ● prod-db1        ssh · andreas@10.0.0.12
    ○ staging-api     ssh
▸ Personal (2)
▸ Ungrouped (7)
```

Written in Rust, one binary, no daemon. Each launch reads the config, asks tmux
for its state, renders, acts and exits.

## Install

```bash
cargo install --path .
```

For a fully static binary: `cargo build --release --target x86_64-unknown-linux-musl`.

Add the popup binding to `~/.tmux.conf`:

```tmux
bind Space display-popup -E -w 60% -h 70% hop
```

## How entries map to tmux

| Entry | Example | tmux |
| --- | --- | --- |
| Local project | `portal-scraper` | session `portal-scraper` with its windows (editor, shell, logs) |
| Service | `postgres` | session `db-postgres` running its start command |
| SSH host | `prod-db1` in group Work | window `prod-db1` in session `ssh-work`, running `ssh prod-db1` |

* Lookups are exact (`-t =name`), so `db` never matches `db-postgres`.
* Names are made tmux-safe: `.`, `:` and spaces become `-`.
* An entry can be in several groups. Its first group (YAML order, then
  `# hop:` comments) decides its SSH session.
* Project/service commands are typed into a shell, so the window stays when
  the command exits. SSH windows close when ssh exits, unless
  `keep_on_exit: true`, which shows `[exited] r: reconnect` instead.
* `own_session: true` gives an SSH host its own session (`ssh-<alias>`) for
  when you want several panes.

## Config

`~/.config/hop/config.yaml` (or `$HOP_CONFIG`) holds groups, projects and
services. See [`examples/config.yaml`](examples/config.yaml). Every key is
optional; with no config file at all, `hop` lists your ssh hosts under
"Ungrouped".

Hosts come from `~/.ssh/config`, which `hop` only reads. `Include` is
followed and wildcard-only blocks (`Host *`) are skipped. Tag a host with a
comment just above it and it shows up without touching the YAML:

```
# hop: group=Work tags=db,prod
Host prod-db1
    HostName 10.0.0.12
    User andreas
```

The annotation also accepts `group=A,B`, `keep_on_exit=true` and
`own_session=true`. Options set in YAML override it.

## Keys

| Key | Action |
| --- | --- |
| `Enter` | Attach or create, then close the popup (on a group: expand/collapse) |
| `j / k`, `h / l` | Move; collapse or expand a group |
| `/` | Fuzzy filter across names, hosts and tags; matching groups expand |
| `Space` | Mark entries; `Enter` then opens all of them (`Tab` while filtering) |
| `v / s` | Open an SSH host as a side-by-side / stacked split in the current window |
| `x` | Kill the session or window, after `y` to confirm |
| `e` | Edit the config in `$VISUAL`/`$EDITOR`, then reload |
| `r` | Reload config |
| `Esc / q` | Clear the filter or marks / close |

Expanded groups are remembered in `~/.local/state/hop/state.json`.

## CLI

```bash
hop prod-db1                 # attach-or-create without the UI
hop portal-scraper postgres  # several; the last one gets focus
hop --list                   # name, kind, tmux target, running/stopped
```

Names match the entry name, or its tmux name (`db-postgres`,
`ssh-work:prod-db1`). Outside tmux, `hop` attaches to the result. Closing
the picker there drops you into a `home` session, so the very first launch
works too.

## i3

[`contrib/hop-term`](contrib/hop-term) focuses the one terminal with class
`hop`, or launches it attached to tmux. Bindings are in
[`contrib/i3.conf`](contrib/i3.conf):

```
for_window [class="hop"] move to workspace 1:term
bindsym $mod+Return exec hop-term
bindsym $mod+space  exec hop-term && tmux display-popup -E -w 60% -h 70% hop
```

## Development

```bash
cargo test
```

`src/tmux.rs` is the only module with side effects. Its tests run against an
isolated server (`tmux -L hop-test-*`), so they never touch your sessions.
Set `HOP_TMUX_SOCKET` to point the binary itself at a scratch server.

| Module | Job |
| --- | --- |
| `main.rs` | Flags: picker or `hop <name>` CLI |
| `config/` | YAML, ssh_config (`Include`, `# hop:` comments), merged into one tree |
| `target.rs` | Entry → tmux session/window and how to create it |
| `tui/` | Tree model, expand state, fuzzy filter, keys (`model.rs` is pure) |
| `tmux.rs` | `list-windows`, `ensure`, `switch`, `attach`, `kill`, `split` |

Not yet built (M4): reachability dots for SSH hosts, recently used at the
top, hot reload.
