//! Rendering of the picker.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::model::{Mode, Model, Row};

const MAX_NAME: usize = 28;

pub fn draw(f: &mut Frame, m: &Model, state: &mut ListState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    // Header: filter prompt or title.
    let head = if m.mode == Mode::Filter || !m.query.is_empty() {
        let mut spans = vec![
            Span::styled("/ ", Style::new().fg(Color::Yellow)),
            Span::raw(m.query.clone()),
        ];
        if m.mode == Mode::Filter {
            spans.push(Span::styled("▏", Style::new().fg(Color::Yellow)));
        }
        Line::from(spans)
    } else {
        let marked = if m.marked.is_empty() {
            String::new()
        } else {
            format!("  · {} marked", m.marked.len())
        };
        Line::from(vec![
            Span::styled("hop", Style::new().bold()),
            Span::styled(
                format!("  {} entries{marked}", m.tree.entries.len()),
                Style::new().dark_gray(),
            ),
        ])
    };
    f.render_widget(Paragraph::new(head), header);

    // Body: the tree.
    let rows = m.rows();
    let width = m
        .tree
        .entries
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_NAME);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match *row {
            Row::Group(g) => {
                let group = &m.tree.groups[g];
                let open = m.is_expanded(&group.name);
                let mut spans = vec![
                    Span::styled(if open { "▾ " } else { "▸ " }, Style::new().dark_gray()),
                    Span::styled(group.name.clone(), Style::new().bold()),
                ];
                if !open {
                    spans.push(Span::styled(
                        format!(" ({})", group.entries.len()),
                        Style::new().dark_gray(),
                    ));
                }
                ListItem::new(Line::from(spans))
            }
            Row::Entry(_, e) => {
                let entry = &m.tree.entries[e];
                let running = m.running(e);
                let marked = m.marked.contains(&e);
                let mut name: String = entry.name.chars().take(MAX_NAME).collect();
                let pad = width.saturating_sub(name.chars().count());
                name.push_str(&" ".repeat(pad));
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if marked { "  ✚ " } else { "    " },
                        Style::new().fg(Color::Magenta),
                    ),
                    if running {
                        Span::styled("●", Style::new().fg(Color::Green))
                    } else {
                        Span::styled("○", Style::new().dark_gray())
                    },
                    Span::raw(" "),
                    Span::styled(
                        name,
                        if marked {
                            Style::new().fg(Color::Magenta)
                        } else {
                            Style::new()
                        },
                    ),
                    Span::raw("  "),
                    Span::styled(m.info(e), Style::new().dark_gray()),
                ]))
            }
        })
        .collect();
    state.select(if rows.is_empty() {
        None
    } else {
        Some(m.cursor)
    });
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    if rows.is_empty() {
        let msg = if m.query.is_empty() {
            "Nothing configured yet. Press e to edit the config, or add hosts to ~/.ssh/config."
        } else {
            "No matches."
        };
        f.render_widget(Paragraph::new(msg).dark_gray(), body);
    } else {
        f.render_stateful_widget(list, body, state);
    }

    // Footer: confirmation, message or key help.
    let foot = match m.mode {
        Mode::Confirm(e) => Line::from(vec![
            Span::styled(
                format!("Kill {}? ", m.targets[e].label()),
                Style::new().fg(Color::Red).bold(),
            ),
            Span::raw("y/N"),
        ]),
        Mode::Filter => Line::from(Span::styled(
            "enter open · ↑↓ move · tab mark · esc clear",
            Style::new().dark_gray(),
        )),
        Mode::Normal => match &m.message {
            Some(msg) => Line::from(Span::styled(msg.clone(), Style::new().fg(Color::Yellow))),
            None => Line::from(Span::styled(
                "enter open · space mark · / filter · v/s split · x kill · e edit · r reload · q quit",
                Style::new().dark_gray(),
            )),
        },
    };
    f.render_widget(Paragraph::new(foot), footer);
}
