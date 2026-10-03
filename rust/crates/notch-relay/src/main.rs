//! notch-relay binary: single small server, one SQLite file, env config only.

use std::path::PathBuf;
use std::time::Duration;

use notch_relay::hub::{KEEPALIVE, MESSAGE_CAP, MIN_RETENTION_SECS};
use notch_relay::{AppState, router};
use tokio::net::TcpListener;

fn main() -> Result<(), String> {
    let db = std::env::var("NOTCH_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| "notch.db".into());
    let addr = std::env::var("NOTCH_ADDR").unwrap_or_else(|_| "0.0.0.0:8899".into());
    let state = AppState::new(db)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        // Housekeeping: prune expired/acked/over-cap rows once a minute.
        {
            let store = state.store.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    let _ = store.prune(MESSAGE_CAP, MIN_RETENTION_SECS);
                }
            });
        }
        let listener = TcpListener::bind(&addr).await.map_err(|e| e.to_string())?;
        let bound = listener.local_addr().map_err(|e| e.to_string())?;
        println!("notch-relay listening on {bound} (keepalive {KEEPALIVE:?})");
        axum::serve(listener, router(state))
            .await
            .map_err(|e| e.to_string())
    })
}
