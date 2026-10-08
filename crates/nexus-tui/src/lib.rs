//! Zeta TUI v5 — 精确复刻 opencode 视觉体系。
//!
//! 配色（从 opencode 1.18.35 真机样式码提取）：
//! - 背景 #0D1117(13,17,23) 铺满、输入区 #161B22(22,27,34)、标题底 #010409(1,4,9)
//! - accent 紫 #BC8CFF(188,140,255)、正文 #C9D1D9(201,209,217)、弱化 #8B949E(139,148,158)
//! - 动画紫 #604B85(96,75,133)、绿 #3FB950(63,185,80)、橙 #E3B341(227,179,65)
//! 布局：左消息流(宽2/3) + 右侧 Context 面板(1/3) + 满宽三行输入框 + 状态行。

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use nexus_agent::{Agent, AgentEvent};
use nexus_core::config::Config;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

const BG: Color = Color::Rgb(13, 17, 23);
const BG_INPUT: Color = Color::Rgb(22, 27, 34);
const ACCENT: Color = Color::Rgb(188, 140, 255);
const TEXT: Color = Color::Rgb(201, 209, 217);
const MUTED: Color = Color::Rgb(139, 148, 158);
const ANIM: Color = Color::Rgb(96, 75, 133);
const GREEN: Color = Color::Rgb(63, 185, 80);
const ORANGE: Color = Color::Rgb(227, 179, 65);
const FAIL: Color = Color::Rgb(248, 81, 73);
const PROMPT: char = '\u{276f}';

pub async fn run_tui(cfg: Config) -> Result<()> {
    let mut terminal = ratatui::init();
    terminal.clear()?;
    let res = tui_loop(cfg, &mut terminal).await;
    ratatui::restore();
    res
}

#[derive(Default)]
struct UiState {
    log: Vec<Line<'static>>,
    input: String,
    status: String,
    metrics: nexus_agent::Metrics,
    started: Option<std::time::Instant>,
    running: bool,
    scroll: u16,
    auto_scroll: bool,
    cancel: bool,
    tool_line: Option<usize>,
    anim_frame: usize,
}

const ANIM_FRAMES: [&str; 4] = ["\u{2839}", "\u{2839}", "\u{283b}", "\u{283f}"];

impl UiState {
    fn push(&mut self, line: Line<'static>) {
        self.log.push(line);
        if self.log.len() > 3000 {
            self.log.drain(..1000);
        }
        if self.auto_scroll {
            self.scroll = 0;
        }
    }
    fn tool_start(&mut self, name: &str, args: &str) {
        let brief: String = args.replace('\n', " ").chars().take(48).collect();
        self.push(Line::from(Span::styled(
            format!("{} {} {}", ANIM_FRAMES[self.anim_frame % 4], name, brief),
            Style::default().fg(ORANGE),
        )));
        self.tool_line = Some(self.log.len() - 1);
    }
    fn tool_end(&mut self, name: &str, preview: &str) {
        let prev: String = preview.replace('\n', " ").chars().take(80).collect();
        let line = Line::from(Span::styled(
            format!("\u{25b8} {} \u{00b7} {}", name, prev),
            Style::default().fg(MUTED),
        ));
        match self.tool_line.take() {
            Some(i) if i < self.log.len() => self.log[i] = line,
            _ => self.push(line),
        }
    }
    fn user(&mut self, s: &str) {
        self.push(Line::from(Span::styled(
            format!("{} {}", PROMPT, s),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
    }
}

enum TaskCtrl {
    Cancel,
}

async fn tui_loop(cfg: Config, terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut st = UiState::default();
    let mut events_rx: Option<mpsc::UnboundedReceiver<AgentEvent>> = None;
    let mut ctrl_tx: Option<mpsc::UnboundedSender<TaskCtrl>> = None;
    let mut event_stream = EventStream::new();
    let mut quit = false;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(120));

    while !quit {
        let mut submitted = false;

        if let Some(rx) = events_rx.as_mut() {
            tokio::select! {
                _ = tick.tick() => { if st.running { st.anim_frame += 1; } }
                maybe = event_stream.next() => term_event(maybe, &mut st, &mut quit, &mut submitted),
                maybe = rx.recv() => match maybe {
                    Some(evt) => apply_event(evt, &mut st),
                    None => { events_rx = None; st.running = false; st.status = "ready".into(); }
                },
            }
        } else {
            tokio::select! {
                _ = tick.tick() => {}
                maybe = event_stream.next() => term_event(maybe, &mut st, &mut quit, &mut submitted),
            }
        }

        if st.cancel {
            st.cancel = false;
            if let Some(tx) = ctrl_tx.take() {
                let _ = tx.send(TaskCtrl::Cancel);
            }
            st.running = false;
            st.status = "cancelled".into();
            st.push(Line::from(Span::styled("cancelled", Style::default().fg(FAIL))));
        }

        if submitted && !st.running {
            let prompt = st.input.trim().to_string();
            if !prompt.is_empty() {
                st.input.clear();
                st.user(&prompt);
                st.running = true;
                st.status = "working".into();
                st.started = Some(std::time::Instant::now());
                let (etx, erx) = mpsc::unbounded_channel::<AgentEvent>();
                let (ctx, crx) = mpsc::unbounded_channel::<TaskCtrl>();
                events_rx = Some(erx);
                ctrl_tx = Some(ctx);
                let cfg2 = cfg.clone();
                tokio::spawn(async move {
                    if let Err(e) = spawn_and_forward(cfg2, prompt, etx, crx).await {
                        tracing::error!("task: {:#}", e);
                    }
                });
            }
        }

        draw(terminal, &st, &cfg)?;
    }
    Ok(())
}

fn term_event(
    maybe: Option<Result<Event, std::io::Error>>,
    st: &mut UiState,
    quit: &mut bool,
    submitted: &mut bool,
) {
    if let Some(Ok(Event::Key(k))) = maybe {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            *quit = true;
        } else if k.code == KeyCode::Esc && st.running {
            st.cancel = true;
        } else if k.code == KeyCode::Up {
            st.auto_scroll = false;
            st.scroll = st.scroll.saturating_add(4);
        } else if k.code == KeyCode::Down {
            st.scroll = st.scroll.saturating_sub(4);
            if st.scroll == 0 {
                st.auto_scroll = true;
                       }
        } else if k.code == KeyCode::Enter {
            *submitted = !st.running && !st.input.trim().is_empty();
        } else if k.code == KeyCode::Backspace {
            st.input.pop();
        } else if let KeyCode::Char(c) = k.code {
            st.input.push(c);
        }
    }
}

async fn spawn_and_forward(
    cfg: Config,
    prompt: String,
    tx: mpsc::UnboundedSender<AgentEvent>,
    mut ctrl: mpsc::UnboundedReceiver<TaskCtrl>,
) -> Result<()> {
    let agent_tx = tx.clone();
    let run = async {
        let mut agent = Agent::new(cfg, agent_tx).await?;
        agent.run_task(&prompt).await
    };
    tokio::select! {
        r = run => match r {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = tx.send(AgentEvent::Error { message: format!("{:#}", e), retry_after_secs: 0 });
                let _ = tx.send(AgentEvent::Done { reason: "failed".into() });
                Err(e)
            }
        },
        _ = ctrl.recv() => {
            let _ = tx.send(AgentEvent::Done { reason: "cancelled".into() });
            Ok(())
        }
    }
}

fn apply_event(evt: AgentEvent, st: &mut UiState) {
    match evt {
        AgentEvent::Delta(t) => {
            for line in t.lines() {
                st.push(Line::from(Span::styled(line.to_string(), Style::default().fg(TEXT))));
            }
        }
        AgentEvent::ReasoningDelta(_) => {}
        AgentEvent::ToolStart { name, args } => st.tool_start(&name, &args),
        AgentEvent::ToolEnd { name, output_preview } => st.tool_end(&name, &output_preview),
        AgentEvent::Acceptance { passed, detail } => {
            if passed {
                st.push(Line::from(Span::styled(
                    "\u{2713} acceptance passed \u{00b7} evidence replayed, exit 0",
                    Style::default().fg(GREEN),
                )));
            } else {
                let d: String = detail.replace('\n', " ").chars().take(110).collect();
                st.push(Line::from(Span::styled(
                    format!("\u{2717} acceptance failed \u{00b7} {}", d),
                    Style::default().fg(FAIL),
                )));
            }
        }
        AgentEvent::Status { metrics } => st.metrics = metrics,
        AgentEvent::Done { reason } => {
            st.tool_line = None;
            if reason != "no more tool calls" {
                let c = if reason == "cancelled" { FAIL } else { GREEN };
                st.push(Line::from(Span::styled(reason, Style::default().fg(c))));
            }
            st.running = false;
            st.status = "ready".into();
        }
        AgentEvent::Error { message, retry_after_secs } => {
            let m: String = message.replace('\n', " ").chars().take(110).collect();
            st.push(Line::from(Span::styled(
                format!("error, retry {}s: {}", retry_after_secs, m),
                Style::default().fg(FAIL),
            )));
        }
    }
}

fn draw(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, st: &UiState, cfg: &Config) -> Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        // 全屏深色背景
        f.render_widget(ratatui::widgets::Block::new().style(Style::default().bg(BG)), area);

        let rows = Layout::vertical([
            Constraint::Min(4),    // 主区
            Constraint::Length(3), // 输入框三行
            Constraint::Length(1), // 键位行
            Constraint::Length(1), // 状态栏
        ])
        .split(area);

        // 主区：左 2/3 消息流 + 右 1/3 面板
        let cols = Layout::horizontal([Constraint::Percentage(72), Constraint::Percentage(28)]).split(rows[0]);
        if st.log.is_empty() {
            let banner = vec![
                Line::from(Span::styled("\u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2584} \u{2588}\u{2580}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580}", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled("\u{2588}  \u{2588} \u{2588}  \u{2588} \u{2588}\u{2580}\u{2580}\u{2580} \u{2588}  \u{2588} \u{2588}    \u{2588}  \u{2588} \u{2588}  \u{2588}", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
                Line::from(Span::styled("\u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588} \u{2588}    \u{2588}\u{2580}\u{2580}\u{2584} \u{2588}\u{2580}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580}\u{2580}", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
            ];
            f.render_widget(Paragraph::new(banner), cols[0]);
        } else {
            let visible = cols[0].height as usize;
            let total = st.log.len();
            let end = total.saturating_sub(st.scroll as usize);
            let start = end.saturating_sub(visible);
            let lines: Vec<Line> = if start < end { st.log[start..end].to_vec() } else { vec![] };
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), cols[0]);
        }

        // 右侧 Context 面板（opencode 同款：标题+值 行距松）
        let m = st.metrics;
        let panel = vec![
            Line::from(Span::styled("Context", Style::default().fg(TEXT).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(format!("{} tokens", m.prompt_tokens), Style::default().fg(TEXT))),
            Line::from(Span::styled(format!("{:.0}% cached", m.cache_hit_rate * 100.0), Style::default().fg(MUTED))),
            Line::from(""),
            Line::from(Span::styled("GPU", Style::default().fg(TEXT).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(format!("{:.0}\u{00b0}C \u{00b7} {}/{} MB", m.gpu_temp, m.gpu_used_mb, m.gpu_total_mb), Style::default().fg(TEXT))),
            Line::from(""),
            Line::from(Span::styled("Session", Style::default().fg(TEXT).add_modifier(Modifier::BOLD))),
            Line::from(Span::styled(format!("{:.0} tok/s \u{00b7} step {}", m.tok_per_sec, m.step), Style::default().fg(TEXT))),
            Line::from(Span::styled(format!("{} memories \u{00b7} {} errors", m.memories, m.errors), Style::default().fg(MUTED))),
        ];
        f.render_widget(Paragraph::new(panel), cols[1]);

        // 输入区三行（满宽，深色底）
        let input_area = Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)]).split(rows[1]);
        let w = input_area[0].width.saturating_sub(2) as usize;

        let mut l1 = vec![Span::styled("\u{2503}", Style::default().bg(BG_INPUT).fg(ACCENT))];
        let fill1 = w.saturating_sub(2);
        if st.running {
            l1.push(Span::styled(format!(" {} ", "\u{2298}"), Style::default().bg(BG_INPUT).fg(ANIM)));
            l1.push(Span::styled(" ".repeat(0), Style::default().bg(BG_INPUT)));
        } else {
            l1.push(Span::styled(format!(" {} ", PROMPT), Style::default().bg(BG_INPUT).fg(ACCENT)));
            let inp: String = st.input.chars().take(fill1.saturating_sub(3)).collect();
            l1.push(Span::styled(inp, Style::default().bg(BG_INPUT).fg(TEXT)));
            l1.push(Span::styled(fill_spaces(fill1), Style::default().bg(BG_INPUT)));
        }
        f.render_widget(Paragraph::new(Line::from(l1)), input_area[0]);

        let mut l2 = vec![Span::styled("\u{2503}", Style::default().bg(BG_INPUT).fg(BG))];
        let model_note = if st.running {
            format!(" working \u{00b7} {}", cfg.provider.model)
        } else {
            format!(" {} \u{00b7} {}", st.status, cfg.provider.model)
        };
        let note_len = model_note.chars().count();
        l2.push(Span::styled(model_note, Style::default().bg(BG_INPUT).fg(MUTED)));
        l2.push(Span::styled(fill_spaces(w.saturating_sub(2 + note_len)), Style::default().bg(BG_INPUT)));
        f.render_widget(Paragraph::new(Line::from(l2)), input_area[1]);

        let rule: String = "\u{2579}".to_string() + &"\u{2580}".repeat(w);
        f.render_widget(
            Paragraph::new(Span::styled(rule, Style::default().fg(BG))),
            input_area[2],
        );

        // 键位行
        let keys_left = if st.running {
            "   esc interrupt"
        } else {
            "   enter submit"
        };
        let keys_line = Line::from(vec![
            Span::styled(ANIM_FRAMES[st.anim_frame % 4].to_string(), Style::default().fg(ANIM)),
            Span::styled(keys_left, Style::default().fg(TEXT)),
            Span::styled("                                tab agents  ctrl+p commands    ", Style::default().fg(MUTED)),
            Span::styled(format!("\u{2022} zeta {}", env!("CARGO_PKG_VERSION")), Style::default().fg(MUTED)),
        ]);
        f.render_widget(Paragraph::new(keys_line), rows[2]);

        // 状态栏
        let secs = st.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let left = format!(
            " {} \u{00b7} cache {:.0}% \u{00b7} {:.0} tok/s \u{00b7} prompt {} tok \u{00b7} compacted {} \u{00b7} sub {} \u{00b7} cron {} \u{00b7} mem {} \u{00b7} err {}",
            st.status, m.cache_hit_rate * 100.0, m.tok_per_sec, m.prompt_tokens, m.compacted_tokens, m.subagents, m.scheduled, m.memories, m.errors
        );
        let right = format!(
            " gpu {:.0}\u{00b0}C \u{00b7} vram {}/{} MB \u{00b7} up {:02}:{:02}:{:02} ",
            m.gpu_temp, m.gpu_used_mb, m.gpu_total_mb, secs / 3600, (secs % 3600) / 60, secs % 60
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(left, Style::default().bg(BG).fg(MUTED)),
                Span::styled(right, Style::default().bg(BG).fg(FAINT_DIM())),
            ])),
            rows[3],
        );
    })?;
    Ok(())
}

fn FAINT_DIM() -> Color {
    Color::Rgb(70, 76, 84)
}

fn fill_spaces(n: usize) -> String {
    " ".repeat(n)
}
