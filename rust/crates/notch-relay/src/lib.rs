//! notch-relay library: publish over HTTP, subscribe over WebSocket, replay
//! on reconnect. The binary is a thin wrapper; everything meaningful lives in
//! the lib so tests (and future embedders) can drive the real router.

pub mod http;
pub mod hub;
pub mod store;
pub mod ws;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use hub::Hub;
use store::Store;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub hub: Arc<Hub>,
}

impl AppState {
    pub fn new(db: PathBuf) -> Result<AppState, String> {
        let store = Arc::new(Store::open(db)?);
        Ok(AppState {
            hub: Arc::new(Hub::default()),
            store,
        })
    }
}

/// Topic validation lives in notch-core; re-exported for callers/tests.
pub use notch_core::valid_topic;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/t/{topic}", get(http::poll).post(http::publish))
        .route("/ws/{topic}", get(ws::handle))
        .route("/health", get(http::health))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_rules() {
        assert!(valid_topic("alerts"));
        assert!(valid_topic("a_B-9"));
        assert!(!valid_topic(""));
        assert!(!valid_topic("has space"));
        assert!(!valid_topic(&"x".repeat(65)));
    }
}
