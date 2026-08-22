use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct WireEvent {
    pub seq: i64,
    pub ts_ms: i64,
    pub kind: String,
    #[schema(value_type = Object)]
    pub data: Value,
}

#[derive(Clone, Default)]
pub struct Bus {
    inner: Arc<DashMap<String, broadcast::Sender<Arc<WireEvent>>>>,
}

impl Bus {
    pub fn register(&self, job_id: &str) {
        let (tx, _rx) = broadcast::channel(4096);
        self.inner.insert(job_id.to_string(), tx);
    }

    pub fn subscribe(&self, job_id: &str) -> Option<broadcast::Receiver<Arc<WireEvent>>> {
        self.inner.get(job_id).map(|e| e.subscribe())
    }

    pub fn send(&self, job_id: &str, event: WireEvent) {
        if let Some(entry) = self.inner.get(job_id) {
            let _ = entry.send(Arc::new(event));
        }
    }

    pub fn remove(&self, job_id: &str) {
        self.inner.remove(job_id);
    }
}
