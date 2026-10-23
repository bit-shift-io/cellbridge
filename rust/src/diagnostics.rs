//! Bounded metadata-only events; no configuration or packet payloads.
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEvent {
    pub timestamp_ms: u64,
    pub component: String,
    pub message: String,
}

fn events() -> &'static Mutex<VecDeque<DiagnosticEvent>> {
    static EVENTS: OnceLock<Mutex<VecDeque<DiagnosticEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub(crate) fn record(component: &str, message: &str) {
    let mut events = events().lock().unwrap_or_else(|e| e.into_inner());
    if events.len() == 256 {
        events.pop_front();
    }
    events.push_back(DiagnosticEvent {
        timestamp_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        component: component.into(),
        message: message.into(),
    });
}

pub fn snapshot() -> Vec<DiagnosticEvent> {
    events()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn event_history_is_bounded_and_keeps_latest() {
        for n in 0..300 {
            super::record("test", &n.to_string());
        }
        let events = super::snapshot();
        assert_eq!(events.len(), 256);
        assert_eq!(events.last().unwrap().message, "299");
    }
}
