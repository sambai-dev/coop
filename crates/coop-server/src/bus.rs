use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct WireEvent {
    pub seq: i64,
    pub ts_ms: i64,
    pub kind: String,
    #[schema(value_type = Object)]
    pub data: Value,
    pub prev_hash: String,
    pub event_hash: String,
    pub hash_version: i64,
}

#[derive(Clone, Default)]
pub struct Bus {
    inner: Arc<DashMap<String, JobChannel>>,
}

#[derive(Clone)]
struct JobChannel {
    events: broadcast::Sender<Arc<WireEvent>>,
    terminal: watch::Sender<bool>,
}

impl Bus {
    pub fn register(&self, job_id: &str) {
        let (events, _rx) = broadcast::channel(4096);
        let (terminal, _terminal_rx) = watch::channel(false);
        self.inner
            .insert(job_id.to_string(), JobChannel { events, terminal });
    }

    pub fn subscribe(&self, job_id: &str) -> Option<broadcast::Receiver<Arc<WireEvent>>> {
        self.inner.get(job_id).map(|entry| entry.events.subscribe())
    }

    /// Subscribe to terminal-state notification without polling SQLite. The
    /// watch value is sticky for receivers that subscribed before completion.
    pub fn completion(&self, job_id: &str) -> Option<watch::Receiver<bool>> {
        self.inner
            .get(job_id)
            .map(|entry| entry.terminal.subscribe())
    }

    pub fn send(&self, job_id: &str, event: WireEvent) {
        if let Some(entry) = self.inner.get(job_id) {
            let _ = entry.events.send(Arc::new(event));
        }
    }

    /// Wake result waiters, close the live-event channel, and release the map
    /// entry. Existing watch receivers retain the terminal `true` value.
    pub fn complete(&self, job_id: &str) {
        if let Some((_, channel)) = self.inner.remove(job_id) {
            channel.terminal.send_replace(true);
        }
    }

    pub fn remove(&self, job_id: &str) {
        self.complete(job_id);
    }
}
