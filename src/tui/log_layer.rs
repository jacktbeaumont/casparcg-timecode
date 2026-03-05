//! Tracing layer that routes log events to the TUI via a mpsc channel.

use super::state::{LogEntry, UiMessage};
use tokio::sync::mpsc;

pub struct TuiLogLayer {
    tx: mpsc::Sender<UiMessage>,
}

impl TuiLogLayer {
    pub fn new(tx: mpsc::Sender<UiMessage>) -> Self {
        Self { tx }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for TuiLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let entry = LogEntry {
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            level,
            message: visitor.formatted(),
        };

        let _ = self.tx.try_send(UiMessage::Log(entry));
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            name => {
                if !self.fields.is_empty() {
                    self.fields.push(' ');
                }
                self.fields.push_str(&format!("{name}={value:?}"));
            }
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            name => {
                if !self.fields.is_empty() {
                    self.fields.push(' ');
                }
                self.fields.push_str(&format!("{name}={value}"));
            }
        }
    }
}

impl MessageVisitor {
    fn formatted(&self) -> String {
        if self.fields.is_empty() {
            self.message.clone()
        } else {
            format!("{}  {}", self.message, self.fields)
        }
    }
}
