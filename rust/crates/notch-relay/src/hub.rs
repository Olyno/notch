//! Live subscriber registry. The hub never blocks on a slow socket: each
//! subscriber owns a bounded writer fed by a channel; a full queue means the
//! client cannot keep up, so the sender is dropped, the writer task ends and
//! the socket closes — the client must reconnect with its cursor, and replay
//! makes that lossless.

use notch_core::Filter;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

use crate::store::Store;

/// Keepalive ping interval; client pongs reset the liveness clock.
pub const KEEPALIVE: Duration = Duration::from_secs(25);
/// Bounded queue per subscriber: ~2 pages of replay before backpressure.
pub const SUBSCRIBER_QUEUE: usize = 400;

/// Per-topic retention cap (messages) and the min age before acked rows prune.
pub const MESSAGE_CAP: i64 = 5000;
pub const MIN_RETENTION_SECS: i64 = 600;

pub struct Subscriber {
    pub id: u64,
    pub topic: String,
    pub filter: Filter,
    pub store: Arc<Store>,
    tx: Mutex<Option<mpsc::Sender<String>>>,
    /// Set when the hub evicts this subscriber (queue overflow): the WS task
    /// must stop sending and close the socket instead of draining the queue.
    dead: AtomicBool,
}

impl Subscriber {
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    /// Best-effort delivery of raw JSON. Returns false when this subscriber
    /// was killed (queue full): the caller must remove it from the registry.
    pub fn push(&self, json: &str) -> bool {
        let mut guard = self.tx.lock();
        if let Some(tx) = guard.as_ref()
            && let Err(err) = tx.try_send(json.to_owned())
        {
            match err {
                mpsc::error::TrySendError::Closed(_) => {} // task already gone
                mpsc::error::TrySendError::Full(_) => {
                    self.dead.store(true, Ordering::Relaxed);
                    *guard = None; // closes the receiver -> writer task exits
                }
            }
            return false;
        }
        true
    }

    /// Handle one inbound WS text frame: only `{"ack":seq}` is meaningful.
    pub fn handle_inbound(&self, text: &str) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text)
            && let Some(seq) = v.get("ack").and_then(|s| s.as_u64())
        {
            let _ = self.store.ack(&self.topic, &self.id.to_string(), seq);
        }
    }
}

#[derive(Default)]
pub struct Hub {
    by_topic: Mutex<HashMap<String, Vec<Arc<Subscriber>>>>,
    next_id: AtomicU64,
    /// Fires on every publish so HTTP long-poll waiters re-check the store.
    wake: Notify,
}

impl Hub {
    pub fn add(
        &self,
        topic: String,
        filter: Filter,
        store: Arc<Store>,
    ) -> (Arc<Subscriber>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let sub = Arc::new(Subscriber {
            id,
            topic: topic.clone(),
            filter,
            store,
            tx: Mutex::new(Some(tx)),
            dead: AtomicBool::new(false),
        });
        self.by_topic
            .lock()
            .entry(topic)
            .or_default()
            .push(sub.clone());
        (sub, rx)
    }

    pub fn remove(&self, sub: &Arc<Subscriber>) {
        let mut m = self.by_topic.lock();
        if let Some(vec) = m.get_mut(&sub.topic) {
            vec.retain(|s| s.id != sub.id);
            if vec.is_empty() {
                m.remove(&sub.topic);
            }
        }
    }

    pub fn count(&self, topic: &str) -> usize {
        self.by_topic.lock().get(topic).map_or(0, |v| v.len())
    }

    /// Deliver raw JSON to every matching live subscriber, then wake waiters.
    pub fn notify(&self, topic: &str, json: &str, tags: &[String]) {
        let subs: Vec<Arc<Subscriber>> =
            self.by_topic.lock().get(topic).cloned().unwrap_or_default();
        for s in subs {
            if s.filter.matches(tags) && !s.push(json) {
                self.remove(&s); // queue overflow: evict immediately, the
                // ws task exits on the closed channel
            }
        }
        self.wake.notify_waiters();
    }

    pub async fn wait(&self) {
        self.wake.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Arc<Store> {
        Arc::new(Store::open(":memory:").unwrap())
    }

    #[tokio::test]
    async fn delivers_matching_filters_only() {
        let hub = Hub::default();
        let s = store();
        let (sub, mut rx) = hub.add(
            "t".into(),
            Filter {
                any: vec!["join".into()],
                none: vec!["leave".into()],
                all: vec![],
            },
            s.clone(),
        );
        let (_sub2, _rx2) = hub.add("t".into(), Filter::default(), s.clone());
        hub.notify("t", r#"{"seq":1}"#, &["join".into()]);
        assert_eq!(rx.recv().await.unwrap(), r#"{"seq":1}"#);
        hub.notify("t", r#"{"seq":2}"#, &["leave".into()]);
        assert!(rx.try_recv().is_err(), "leave must be filtered server-side");
        assert_eq!(hub.count("t"), 2);
        hub.remove(&sub);
        assert_eq!(hub.count("t"), 1);
    }

    #[tokio::test]
    async fn full_queue_closes_writer() {
        let hub = Hub::default();
        let (_sub, mut rx) = hub.add("t".into(), Filter::default(), store());
        for i in 0..SUBSCRIBER_QUEUE + 5 {
            hub.notify("t", &format!(r#"{{"seq":{i}}}"#), &[]);
        }
        // drained side sees the queued page, then the channel ends
        let mut n = 0;
        while rx.recv().await.is_some() {
            n += 1;
        }
        assert_eq!(n, SUBSCRIBER_QUEUE);
    }

    #[tokio::test]
    async fn ack_persists_to_store() {
        let s = store();
        let hub = Hub::default();
        let (sub, _rx) = hub.add("t".into(), Filter::default(), s.clone());
        sub.handle_inbound(r#"{"ack":9}"#);
        sub.handle_inbound("not json"); // tolerated, no panic
        assert_eq!(s.global_acked("t"), 9);
    }
}
