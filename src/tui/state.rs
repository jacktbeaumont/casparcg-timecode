//! TUI-local runtime state, updated via messages from the main loop.

use crate::config::LayerId;
use crate::media_controller::LayerState;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

pub const MAX_LOGS: usize = 200;

/// Log entry for display in the TUI log panel.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: String,
    pub level: tracing::Level,
    pub message: String,
}

/// Status of the timecode feed, for display in the TUI.
#[derive(Debug, Clone, PartialEq)]
pub enum TcStatus {
    Playing,
    Paused,
}

#[derive(Debug, Clone)]
pub struct LayerDisplay {
    pub id: u16,
    pub state: LayerState,
}

/// Builds a sorted list of [`LayerDisplay`] from the media controller's layer states.
pub fn layer_displays(states: &HashMap<LayerId, LayerState>) -> Vec<LayerDisplay> {
    let mut layers: Vec<LayerDisplay> = states
        .iter()
        .map(|(id, state)| LayerDisplay {
            id: **id,
            state: state.clone(),
        })
        .collect();
    layers.sort_by_key(|l| l.id);
    layers
}

#[derive(Clone)]
pub struct AppState {
    /// Current timecode string, e.g. `"01:02:34:15"`.
    pub tc: String,
    pub tc_status: TcStatus,
    /// Layer states, sorted by layer id.
    pub layers: Vec<LayerDisplay>,
    /// Recent log entries, newest first.
    pub logs: VecDeque<LogEntry>,
    /// Monotonic timestamp of the last timecode event.
    // TODO remove this
    pub last_tc_update: Option<Instant>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            tc: "--:--:--:--".to_string(),
            tc_status: TcStatus::Paused,
            layers: Vec::new(),
            logs: VecDeque::new(),
            last_tc_update: None,
        }
    }

    pub fn push_log(&mut self, entry: LogEntry) {
        self.logs.push_front(entry);
        if self.logs.len() > MAX_LOGS {
            self.logs.pop_back();
        }
    }

    /// Apply a [`UiMessage`] to update the local state.
    pub fn apply(&mut self, msg: UiMessage) {
        match msg {
            UiMessage::Timecode { tc, status } => {
                self.tc = tc;
                self.tc_status = status;
                self.last_tc_update = Some(Instant::now());
            }
            UiMessage::Layers(layers) => {
                self.layers = layers;
            }
            UiMessage::Log(entry) => {
                self.push_log(entry);
            }
        }
    }
}

/// Message to update the UI state.
pub enum UiMessage {
    Timecode { tc: String, status: TcStatus },
    Layers(Vec<LayerDisplay>),
    Log(LogEntry),
}
