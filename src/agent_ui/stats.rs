use std::collections::HashMap;
use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table, Wrap};
use ratatui::Terminal;

use crate::repository::Repository;
use crate::risk::{self, RiskLevel};
use crate::theme::theme;
use crate::util::{dirs_home, format_duration_ms, shorten_path};

use super::{format_datetime, format_full_datetime, load_entries, truncate, Period};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatsFocus {
    Cards,
    HighRisk,
}

struct HighRiskEntry {
    command: String,
    cwd: String,
    started_at: i64,
    exit_code: Option<i32>,
    level: RiskLevel,
}

struct AgentStat {
    name: String,
    total: usize,
    success: usize,
    avg_duration_ms: i64,
    high_risk: usize,
    pkg_count: usize,
    top_dirs: Vec<(String, usize)>,
    high_risk_cmds: Vec<HighRiskEntry>,
}

/// Per-executor accumulator for single-pass stats aggregation.
struct AgentStatBuilder {
    total: usize,
    success: usize,
    total_duration: i64,
    high_risk: usize,
    pkg_count: usize,
    dir_counts: HashMap<String, usize>,
    high_risk_cmds: Vec<HighRiskEntry>,
}

struct AgentStatsApp {
    agents: Vec<AgentStat>,
    period: Period,
    selected: usize, // Which agent card
    focus: StatsFocus,
    risk_selected: usize, // Which high risk row
    cli_executor: Option<String>,
    status_message: Option<(String, std::time::Instant)>,
}

impl AgentStatsApp {
    fn new(repo: &Repository, days: usize, executor: Option<&str>) -> Self {
        let period = match days {
            d if d <= 7 => Period::Days7,
            d if d <= 30 => Period::Days30,
            _ => Period::AllTime,
        };
        let agents = Self::compute(repo, period, executor);
        Self {
            agents,
            period,
            selected: 0,
            focus: StatsFocus::Cards,
            risk_selected: 0,
            cli_executor: executor.map(String::from),
            status_message: None,
        }
    }

    /// Single-pass streaming aggregation: iterates entries once, accumulating
    /// per-executor stats without cloning entries into a `HashMap<String, Vec<Entry>>`.
    fn compute(repo: &Repository, period: Period, executor: Option<&str>) -> Vec<AgentStat> {
        let entries = load_entries(repo, period.after_ms(), executor, None);

        let mut builders: HashMap<String, AgentStatBuilder> = HashMap::new();

        for e in &entries {
            let name = e.executor.as_deref().unwrap_or("unknown");
            let b = builders
                .entry(name.to_string())
                .or_insert_with(|| AgentStatBuilder {
                    total: 0,
                    success: 0,
                    total_duration: 0,
                    high_risk: 0,
                    pkg_count: 0,
                    dir_counts: HashMap::new(),
                    high_risk_cmds: Vec::new(),
                });

            b.total += 1;
            if e.exit_code == Some(0) {
                b.success += 1;
            }
            b.total_duration = b.total_duration.saturating_add(e.duration_ms);

            // Risk assessment (single call gives both level + details)
            if let Some(a) = risk::assess_risk(&e.command) {
                if a.level >= RiskLevel::High {
                    b.high_risk += 1;
                    b.high_risk_cmds.push(HighRiskEntry {
                        command: e.command.clone(),
                        cwd: e.cwd.clone(),
                        started_at: e.started_at,
                        exit_code: e.exit_code,
                        level: a.level,
                    });
                }
            }

            // Package extraction (avoids needing session_risk per group)
            if let Some(pkg) = risk::extract_packages(&e.command) {
                b.pkg_count += pkg.packages.len();
            }

            *b.dir_counts.entry(e.cwd.clone()).or_default() += 1;
        }

        #[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
        let mut result: Vec<AgentStat> = builders
            .into_iter()
            .map(|(name, b)| {
                let avg_duration_ms = if b.total > 0 {
                    b.total_duration / b.total as i64
                } else {
                    0
                };
                let mut top_dirs: Vec<_> = b.dir_counts.into_iter().collect();
                top_dirs.sort_by(|a, b| b.1.cmp(&a.1));
                top_dirs.truncate(10);

                let mut high_risk_cmds = b.high_risk_cmds;
                high_risk_cmds.sort_by(|a, b| b.started_at.cmp(&a.started_at));
                high_risk_cmds.truncate(20);

                AgentStat {
                    name,
                    total: b.total,
                    success: b.success,
                    avg_duration_ms,
                    high_risk: b.high_risk,
                    pkg_count: b.pkg_count,
                    top_dirs,
                    high_risk_cmds,
                }
            })
            .collect();

        result.sort_by(|a, b| b.total.cmp(&a.total));
        result
    }

    fn reload(&mut self, repo: &Repository) {
        self.agents = Self::compute(repo, self.period, self.cli_executor.as_deref());
        if self.selected >= self.agents.len() {
            self.selected = 0;
        }
        self.risk_selected = 0;
    }

    fn selected_high_risk_count(&self) -> usize {
        self.agents
            .get(self.selected)
            .map_or(0, |a| a.high_risk_cmds.len())
    }

    fn handle_input(&mut self, key: crossterm::event::KeyEvent, repo: &Repository) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Char('1') => {
                self.period = Period::Today;
                self.reload(repo);
            }
            KeyCode::Char('2') => {
                self.period = Period::Days7;
                self.reload(repo);
            }
            KeyCode::Char('3') => {
                self.period = Period::Days30;
                self.reload(repo);
            }
            KeyCode::Char('4') => {
                self.period = Period::AllTime;
                self.reload(repo);
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    StatsFocus::Cards => StatsFocus::HighRisk,
                    StatsFocus::HighRisk => StatsFocus::Cards,
                };
                self.risk_selected = 0;
            }
            _ => match self.focus {
                StatsFocus::Cards => match key.code {
                    KeyCode::Left | KeyCode::Char('h') => {
                        self.selected = self.selected.saturating_sub(1);
                        self.risk_selected = 0;
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        if !self.agents.is_empty() {
                            self.selected = (self.selected + 1).min(self.agents.len() - 1);
                            self.risk_selected = 0;
                        }
                    }
                    _ => {}
                },
                StatsFocus::HighRisk => match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.risk_selected = self.risk_selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let max = self.selected_high_risk_count().saturating_sub(1);
                        self.risk_selected = self.risk_selected.saturating_add(1).min(max);
                    }
                    KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(agent) = self.agents.get(self.selected) {
                            if let Some(hr) = agent.high_risk_cmds.get(self.risk_selected) {
                                let msg = arboard::Clipboard::new()
                                    .and_then(|mut clip| clip.set_text(hr.command.clone()))
                                    .map_or_else(
                                        |e| format!("Clipboard error: {e}"),
                                        |()| "Copied!".into(),
                                    );
                                self.status_message = Some((msg, std::time::Instant::now()));
                            }
                        }
                    }
                    _ => {}
                },
            },
        }
        true
    }

    fn render(&self, f: &mut ratatui::Frame) {
        let t = theme();
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(9), // agent cards
                Constraint::Min(6),    // bottom: dirs + high risk
                Constraint::Length(1), // footer
            ])
            .split(size);

        // Header
        let mut header_spans = vec![
            Span::styled(
                " AGENT ANALYTICS ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
        ];
        for (i, p) in [
            Period::Today,
            Period::Days7,
            Period::Days30,
            Period::AllTime,
        ]
        .iter()
        .enumerate()
        {
            let active = *p == self.period;
            header_spans.push(Span::styled(
                format!("{}", i + 1),
                Style::default().fg(t.text_muted),
            ));
            header_spans.push(Span::styled(
                format!(" {} ", p.label()),
                if active {
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.text_secondary)
                },
            ));
        }
        f.render_widget(Paragraph::new(Line::from(header_spans)), chunks[0]);

        // Agent cards
        if self.agents.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "  No agent commands found for this period.",
                    Style::default().fg(t.text_muted),
                )),
                chunks[1],
            );
        } else {
            self.render_agent_cards(f, chunks[1], t);
        }

        // Bottom: dirs (left) | high risk with detail (right)
        if let Some(agent) = self.agents.get(self.selected) {
            let home = dirs_home();
            let bottom_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(chunks[2]);

            self.render_top_dirs(f, bottom_cols[0], t, agent, &home);
            self.render_high_risk_section(f, bottom_cols[1], t, agent, &home);
        }

        // Footer
        let badge_key = Style::default()
            .fg(t.bg_elevated)
            .bg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let badge_label = Style::default().fg(t.text_muted);
        let focus_label = match self.focus {
            StatsFocus::Cards => " High Risk ",
            StatsFocus::HighRisk => " Cards ",
        };
        let mut footer = vec![
            Span::styled(" 1-4 ", badge_key),
            Span::styled(" Period  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(focus_label, badge_label),
            Span::styled(
                if self.focus == StatsFocus::Cards {
                    " ←→ "
                } else {
                    " ↑↓ "
                },
                badge_key,
            ),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" ^Y ", badge_key),
            Span::styled(" Copy  ", badge_label),
            Span::styled(" q ", badge_key),
            Span::styled(" Quit ", badge_label),
        ];

        if let Some((msg, time)) = &self.status_message {
            if time.elapsed() < std::time::Duration::from_secs(2) {
                footer.push(Span::styled(
                    format!(" {msg} "),
                    Style::default().fg(t.success).add_modifier(Modifier::BOLD),
                ));
            }
        }

        f.render_widget(Paragraph::new(Line::from(footer)), chunks[3]);
    }

    fn render_agent_cards(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let card_constraints: Vec<Constraint> =
            self.agents.iter().map(|_| Constraint::Min(20)).collect();
        let card_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(card_constraints)
            .split(area);

        for (i, agent) in self.agents.iter().enumerate() {
            if i >= card_areas.len() {
                break;
            }
            let is_selected = i == self.selected && self.focus == StatsFocus::Cards;
            let border_style = if is_selected {
                Style::default().fg(t.primary)
            } else if i == self.selected {
                Style::default().fg(t.text_secondary)
            } else {
                Style::default().fg(t.border)
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(format!(" {} ", agent.name))
                .title_style(if i == self.selected {
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.text_secondary)
                });

            let inner = block.inner(card_areas[i]);
            f.render_widget(block, card_areas[i]);

            #[allow(clippy::cast_precision_loss)]
            let rate = if agent.total > 0 {
                agent.success as f64 / agent.total as f64 * 100.0
            } else {
                0.0
            };

            let lines = vec![
                Line::from(vec![
                    Span::styled("Commands:  ", Style::default().fg(t.text_muted)),
                    Span::styled(format!("{}", agent.total), Style::default().fg(t.text)),
                ]),
                Line::from(vec![
                    Span::styled("Success:   ", Style::default().fg(t.text_muted)),
                    Span::styled(format!("{rate:.1}%"), Style::default().fg(t.text)),
                ]),
                Line::from(vec![
                    Span::styled("Avg dur:   ", Style::default().fg(t.text_muted)),
                    Span::styled(
                        format_duration_ms(agent.avg_duration_ms),
                        Style::default().fg(t.text),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("High risk: ", Style::default().fg(t.text_muted)),
                    Span::styled(
                        format!("{}", agent.high_risk),
                        if agent.high_risk > 0 {
                            Style::default().fg(t.risk_high)
                        } else {
                            Style::default().fg(t.text)
                        },
                    ),
                ]),
                Line::from(vec![
                    Span::styled("Packages:  ", Style::default().fg(t.text_muted)),
                    Span::styled(format!("{}", agent.pkg_count), Style::default().fg(t.text)),
                ]),
            ];
            f.render_widget(Paragraph::new(lines), inner);
        }
    }

    #[allow(clippy::unused_self)]
    fn render_top_dirs(
        &self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        agent: &AgentStat,
        home: &str,
    ) {
        let dir_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(format!(" Top Directories ({}) ", agent.name))
            .title_style(
                Style::default()
                    .fg(t.text_secondary)
                    .add_modifier(Modifier::BOLD),
            );
        let dir_inner = dir_block.inner(area);
        f.render_widget(dir_block, area);

        let dir_rows: Vec<Row> = agent
            .top_dirs
            .iter()
            .enumerate()
            .map(|(i, (dir, count))| {
                Row::new(vec![
                    Cell::from(format!(" {}.", i + 1)).style(Style::default().fg(t.text_muted)),
                    Cell::from(shorten_path(dir, home)).style(Style::default().fg(t.primary)),
                    Cell::from(format!("{count}")).style(Style::default().fg(t.text)),
                ])
            })
            .collect();
        let dir_widths = [
            Constraint::Length(4),
            Constraint::Min(15),
            Constraint::Length(6),
        ];
        f.render_widget(Table::new(dir_rows, dir_widths), dir_inner);
    }

    fn render_high_risk_section(
        &self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        agent: &AgentStat,
        home: &str,
    ) {
        let in_focus = self.focus == StatsFocus::HighRisk;
        let has_selection = in_focus && !agent.high_risk_cmds.is_empty();

        let sections = if has_selection {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(100)])
                .split(area)
        };

        let border_color = if in_focus { t.primary } else { t.border };
        let risk_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .title(format!(" High Risk Commands ({}) ", agent.name))
            .title_style(Style::default().fg(t.warning).add_modifier(Modifier::BOLD));
        let risk_inner = risk_block.inner(sections[0]);
        f.render_widget(risk_block, sections[0]);

        if agent.high_risk_cmds.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled(
                    "  No high-risk commands",
                    Style::default().fg(t.text_muted),
                )),
                risk_inner,
            );
            return;
        }

        let risk_rows = self.build_high_risk_rows(agent, in_focus, t);
        let risk_widths = [
            Constraint::Length(9),
            Constraint::Min(15),
            Constraint::Length(12),
            Constraint::Length(2),
        ];
        f.render_widget(Table::new(risk_rows, risk_widths), risk_inner);

        if has_selection {
            self.render_high_risk_detail(f, sections[1], t, agent, home);
        }
    }

    fn build_high_risk_rows(
        &self,
        agent: &AgentStat,
        in_focus: bool,
        t: &crate::theme::Theme,
    ) -> Vec<Row<'_>> {
        agent
            .high_risk_cmds
            .iter()
            .enumerate()
            .map(|(i, hr)| {
                let is_sel = in_focus && i == self.risk_selected;
                let base = if is_sel {
                    Style::default().bg(t.selection_bg).fg(t.selection_fg)
                } else {
                    Style::default()
                };

                let level_style = if is_sel {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    match hr.level {
                        RiskLevel::Critical => Style::default().fg(t.risk_critical),
                        _ => Style::default().fg(t.risk_high),
                    }
                };
                let status = match hr.exit_code {
                    Some(0) => "✔",
                    Some(_) => "✘",
                    None => "○",
                };

                let cmd_cell = if is_sel {
                    Cell::from(truncate(&hr.command, 30)).style(base)
                } else {
                    Cell::from(crate::util::highlight_command(
                        &truncate(&hr.command, 30),
                        0,
                    ))
                };

                Row::new(vec![
                    Cell::from(format!("{:>8}", hr.level)).style(level_style),
                    cmd_cell,
                    Cell::from(format_datetime(hr.started_at)).style(if is_sel {
                        base
                    } else {
                        Style::default().fg(t.text_secondary)
                    }),
                    Cell::from(status).style(if is_sel {
                        base
                    } else {
                        match hr.exit_code {
                            Some(0) => Style::default().fg(t.success),
                            Some(_) => Style::default().fg(t.error),
                            None => Style::default().fg(t.text_muted),
                        }
                    }),
                ])
            })
            .collect()
    }

    fn render_high_risk_detail(
        &self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        agent: &AgentStat,
        home: &str,
    ) {
        let Some(hr) = agent.high_risk_cmds.get(self.risk_selected) else {
            return;
        };

        let detail_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary))
            .title(" Command Detail ")
            .title_style(
                Style::default()
                    .fg(t.text_secondary)
                    .add_modifier(Modifier::BOLD),
            );
        let detail_inner = detail_block.inner(area);
        f.render_widget(detail_block, area);

        let label = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let val = Style::default().fg(t.text);

        let risk_color = match hr.level {
            RiskLevel::Critical => t.risk_critical,
            _ => t.risk_high,
        };

        let exit_str = match hr.exit_code {
            Some(0) => "✔ success".to_string(),
            Some(c) => format!("✘ {c} (failed)"),
            None => "○ unknown".to_string(),
        };
        let exit_style = match hr.exit_code {
            Some(0) => Style::default().fg(t.success),
            Some(_) => Style::default().fg(t.error),
            None => Style::default().fg(t.text_muted),
        };

        let cmd_width = detail_inner.width.saturating_sub(2) as usize;
        let cmd_highlighted = crate::util::highlight_command(&hr.command, cmd_width);

        let mut lines = vec![Line::from(Span::styled("Command", label))];
        lines.extend(cmd_highlighted.lines);
        lines.extend([
            Line::from(""),
            Line::from(vec![
                Span::styled("Path  ", label),
                Span::styled(shorten_path(&hr.cwd, home), val),
            ]),
            Line::from(vec![
                Span::styled("Time  ", label),
                Span::styled(format_full_datetime(hr.started_at), val),
            ]),
            Line::from(vec![
                Span::styled("Exit  ", label),
                Span::styled(exit_str, exit_style),
            ]),
            Line::from(vec![
                Span::styled("Risk  ", label),
                Span::styled(
                    format!("{} {}", hr.level.icon(), hr.level),
                    Style::default().fg(risk_color),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "^Y Copy command",
                Style::default().fg(t.text_muted),
            )),
        ]);

        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            detail_inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stats_app(agents: Vec<AgentStat>) -> AgentStatsApp {
        AgentStatsApp {
            agents,
            period: Period::Days7,
            selected: 0,
            focus: StatsFocus::Cards,
            risk_selected: 0,
            cli_executor: None,
            status_message: None,
        }
    }

    fn make_agent_stat(name: &str, total: usize, high_risk_count: usize) -> AgentStat {
        let high_risk_cmds: Vec<HighRiskEntry> = (0..high_risk_count)
            .map(|i| HighRiskEntry {
                command: format!("rm -rf /danger{i}"),
                cwd: "/tmp".into(),
                started_at: 1_000_000 + i as i64 * 1000,
                exit_code: Some(0),
                level: RiskLevel::High,
            })
            .collect();

        AgentStat {
            name: name.into(),
            total,
            success: total,
            avg_duration_ms: 500,
            high_risk: high_risk_count,
            pkg_count: 0,
            top_dirs: vec![("/tmp".into(), total)],
            high_risk_cmds,
        }
    }

    // ── selected_high_risk_count ──

    #[test]
    fn selected_high_risk_count_no_agents() {
        let app = make_stats_app(vec![]);
        assert_eq!(app.selected_high_risk_count(), 0);
    }

    #[test]
    fn selected_high_risk_count_with_agents() {
        let app = make_stats_app(vec![make_agent_stat("claude", 10, 3)]);
        assert_eq!(app.selected_high_risk_count(), 3);
    }

    #[test]
    fn selected_high_risk_count_second_agent() {
        let mut app = make_stats_app(vec![
            make_agent_stat("claude", 10, 2),
            make_agent_stat("cursor", 5, 4),
        ]);
        app.selected = 1;
        assert_eq!(app.selected_high_risk_count(), 4);
    }

    // ── Focus and navigation ──

    #[test]
    fn focus_starts_on_cards() {
        let app = make_stats_app(vec![make_agent_stat("claude", 10, 0)]);
        assert_eq!(app.focus, StatsFocus::Cards);
    }

    #[test]
    fn selected_starts_at_zero() {
        let app = make_stats_app(vec![
            make_agent_stat("claude", 10, 0),
            make_agent_stat("cursor", 5, 0),
        ]);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn risk_selected_starts_at_zero() {
        let app = make_stats_app(vec![make_agent_stat("claude", 10, 5)]);
        assert_eq!(app.risk_selected, 0);
    }

    // ── AgentStat construction ──

    #[test]
    fn agent_stat_top_dirs_populated() {
        let stat = make_agent_stat("test", 5, 0);
        assert_eq!(stat.top_dirs.len(), 1);
        assert_eq!(stat.top_dirs[0].0, "/tmp");
        assert_eq!(stat.top_dirs[0].1, 5);
    }

    #[test]
    fn agent_stat_high_risk_cmds_match_count() {
        let stat = make_agent_stat("test", 10, 3);
        assert_eq!(stat.high_risk_cmds.len(), 3);
    }

    // ── Empty state ──

    #[test]
    fn empty_agents_app() {
        let app = make_stats_app(vec![]);
        assert!(app.agents.is_empty());
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_high_risk_count(), 0);
    }
}

pub fn run_agent_stats_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    repo: &Repository,
    days: usize,
    executor: Option<&str>,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = AgentStatsApp::new(repo, days, executor);

    loop {
        terminal.draw(|f| app.render(f))?;

        let timeout = if app.status_message.is_some() {
            std::time::Duration::from_secs(2)
        } else {
            std::time::Duration::from_secs(60)
        };
        if !event::poll(timeout)? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if !app.handle_input(key, repo) {
                return Ok(());
            }
        }
    }
}
