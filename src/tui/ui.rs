//! Rendering functions for the TUI layout.

use super::state::{AppState, TcStatus};
use crate::media_controller::LayerState;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

pub fn render(f: &mut ratatui::Frame, state: &AppState) {
    let layer_count = state.layers.len().max(1) as u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),               // timecode
            Constraint::Length(layer_count + 2), // layers (+ top/bottom border)
            Constraint::Min(3),                  // log
        ])
        .split(f.area());

    render_timecode(f, state, chunks[0]);
    render_layers(f, state, chunks[1]);
    render_logs(f, state, chunks[2]);
}

fn render_timecode(f: &mut ratatui::Frame, state: &AppState, area: ratatui::layout::Rect) {
    let (status_label, status_color) = match state.tc_status {
        TcStatus::Playing => ("PLAYING", Color::Green),
        TcStatus::Paused => ("PAUSED", Color::Yellow),
    };

    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", status_label),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let line = Line::from(Span::styled(
        format!("  {}", state.tc),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    f.render_widget(Paragraph::new(line), inner);
}

fn render_layers(f: &mut ratatui::Frame, state: &AppState, area: ratatui::layout::Rect) {
    let block = Block::default().title(" Layers ").borders(Borders::ALL);

    let items: Vec<ListItem> = if state.layers.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  No layers configured",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        state
            .layers
            .iter()
            .map(|layer| {
                let (filename, color) = match &layer.state {
                    LayerState::Playing { filename } => (filename, Color::Green),
                    LayerState::Paused { filename } => (filename, Color::Yellow),
                    LayerState::Stopped => (&"-".to_string(), Color::DarkGray),
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("  L{:<5}", layer.id),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::styled(
                        format!("{:<30}", filename),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                ]);
                ListItem::new(line)
            })
            .collect()
    };

    f.render_widget(List::new(items).block(block), area);
}

fn render_logs(f: &mut ratatui::Frame, state: &AppState, area: ratatui::layout::Rect) {
    let block = Block::default().title(" Log ").borders(Borders::ALL);
    let inner_height = area.height.saturating_sub(2) as usize;

    let items: Vec<ListItem> = state
        .logs
        .iter()
        .take(inner_height)
        .map(|entry| {
            let level_color = match entry.level {
                tracing::Level::ERROR => Color::Red,
                tracing::Level::WARN => Color::Yellow,
                tracing::Level::INFO => Color::Green,
                tracing::Level::DEBUG => Color::Blue,
                tracing::Level::TRACE => Color::Magenta,
            };

            let level_str = match entry.level {
                tracing::Level::ERROR => "ERROR",
                tracing::Level::WARN => "WARN ",
                tracing::Level::INFO => "INFO ",
                tracing::Level::DEBUG => "DEBUG",
                tracing::Level::TRACE => "TRACE",
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{}  ", entry.time),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("{level_str}  "), Style::default().fg(level_color)),
                Span::styled(&*entry.message, Style::default().fg(Color::White)),
            ]))
        })
        .collect();

    f.render_widget(List::new(items).block(block), area);
}
