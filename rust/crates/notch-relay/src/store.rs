//! SQLite-backed message store: per-topic `seq`, retain-until-acked with
//! TTL/cap pruning. A single WAL connection behind a mutex; at this scale
//! µs-fast ops never measurably block the runtime, and one writer avoids
//! `SQLITE_BUSY` entirely.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use notch_core::{Message, Publish};
use parking_lot::Mutex;
use rusqlite::{Connection, params};

pub const DEFAULT_TTL_SECS: i64 = 86_400;
pub const MAX_PAGE: usize = 200;
/// A cursor that has not acked within this window is considered dead and
/// stops pinning the topic's global ack floor (see recompute_floor).
pub const CLIENT_LIVENESS_SECS: i64 = 2 * 60 * 60;
/// Cursors dead longer than this are dropped entirely on prune.
pub const CURSOR_TTL_SECS: i64 = 7 * 24 * 60 * 60;

pub struct Store {
    conn: Mutex<Connection>,
}

/// Result of a page query: raw stored JSON plus the highest seq *scanned*
/// (which continues the replay cursor even when a filter drops rows).
pub struct Page {
    pub messages: Vec<String>,
    pub last_scanned: u64,
}

pub struct Published {
    pub message: Message,
    pub json: String,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS topics(
                 topic TEXT PRIMARY KEY, next_seq INTEGER NOT NULL, global_acked INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS messages(
                 topic TEXT NOT NULL, seq INTEGER NOT NULL, ts INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL, body TEXT NOT NULL,
                 PRIMARY KEY (topic, seq)
             );
             CREATE TABLE IF NOT EXISTS client_cursors(
                 topic TEXT NOT NULL, cid TEXT NOT NULL, acked INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (topic, cid)
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Atomically assign the next seq and insert the serialized message.
    pub fn publish(&self, topic: &str, p: &Publish) -> Result<Published, String> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO topics(topic, next_seq) VALUES(?1, 1) ON CONFLICT(topic) DO NOTHING",
            params![topic],
        )
        .map_err(|e| e.to_string())?;
        let seq: i64 = tx
            .query_row(
                "SELECT next_seq FROM topics WHERE topic=?1",
                params![topic],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let ts = Self::now();
        let ttl = if p.ttl == 0 {
            DEFAULT_TTL_SECS
        } else {
            p.ttl as i64
        };
        let message = p.clone().into_message(topic.to_string(), seq as u64, ts);
        let json = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO messages(topic, seq, ts, expires_at, body) VALUES(?1,?2,?3,?4,?5)",
            params![topic, seq, ts, ts + ttl, json],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE topics SET next_seq=next_seq+1 WHERE topic=?1",
            params![topic],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Published { message, json })
    }

    /// Up to `limit` raw messages with `seq > after`, oldest first.
    pub fn since(&self, topic: &str, after: u64, limit: usize) -> Result<Page, String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT seq, body FROM messages WHERE topic=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![topic, after as i64, limit as i64], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut messages = Vec::with_capacity(limit.min(MAX_PAGE));
        let mut last_scanned = after;
        for row in rows {
            let (seq, body) = row.map_err(|e| e.to_string())?;
            last_scanned = seq;
            messages.push(body);
        }
        Ok(Page {
            messages,
            last_scanned,
        })
    }

    /// The topic's global ack floor (min over registered client cursors).
    pub fn global_acked(&self, topic: &str) -> i64 {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT global_acked FROM topics WHERE topic=?1",
            params![topic],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// Record a client cursor; recomputes the topic's global ack floor.
    pub fn ack(&self, topic: &str, cid: &str, seq: u64) -> Result<(), String> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = Self::now();
        tx.execute(
            "INSERT INTO topics(topic, next_seq) VALUES(?1, 1) ON CONFLICT(topic) DO NOTHING",
            params![topic],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO client_cursors(topic, cid, acked, updated_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(topic,cid) DO UPDATE SET acked=MAX(acked, excluded.acked), updated_at=excluded.updated_at",
            params![topic, cid, seq as i64, now],
        )
        .map_err(|e| e.to_string())?;
        Self::recompute_floor(&tx, topic, now).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Floor = min acked over LIVE cursors (acked recently). A phone that
    /// died is not allowed to pin retention forever; its replay rights end
    /// with the message TTL anyway.
    fn recompute_floor(
        tx: &rusqlite::Transaction<'_>,
        topic: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "UPDATE topics SET global_acked = COALESCE((
                 SELECT MIN(acked) FROM client_cursors
                 WHERE topic=?1 AND updated_at > ?2
             ), 0)",
            params![topic, now - CLIENT_LIVENESS_SECS],
        )?;
        Ok(())
    }

    /// Drop expired rows, rows every stable client acked (past a min
    /// retention window) and rows beyond the per-topic cap.
    pub fn prune(&self, cap: i64, min_retention: i64) -> Result<(), String> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = Self::now();
        // Dead cursors release their pin even without fresh acks: re-age
        // every floor from LIVE cursors only.
        tx.execute(
            "UPDATE topics SET global_acked = COALESCE((
                 SELECT MIN(acked) FROM client_cursors
                 WHERE client_cursors.topic = topics.topic AND updated_at > ?1
             ), 0)",
            params![now - CLIENT_LIVENESS_SECS],
        )
        .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM messages WHERE expires_at < ?1", params![now])
            .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM messages WHERE ts < ?1 - ?2
             AND seq <= (SELECT global_acked FROM topics WHERE topics.topic = messages.topic)",
            params![now, min_retention],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM messages WHERE topic IN (SELECT topic FROM topics)
             AND seq <= (SELECT COALESCE(MAX(seq),0) - ?1 FROM messages m2 WHERE m2.topic = messages.topic)",
            params![cap],
        )
        .map_err(|e| e.to_string())?;
        // GC cursors of devices that never came back (they stopped pinning
        // the floor once stale — see recompute_floor).
        tx.execute(
            "DELETE FROM client_cursors WHERE updated_at < ?1",
            params![now - CURSOR_TTL_SECS],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(test)]
    pub fn publish_simple(&self, topic: &str, title: &str, tags: &[&str]) -> Published {
        let p = Publish {
            title: title.into(),
            body: String::new(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            priority: Default::default(),
            category: String::new(),
            icon: String::new(),
            silent: false,
            ttl: 0,
            timeout_ms: 0,
            dedup_key: String::new(),
            channel: String::new(),
        };
        self.publish(topic, &p).unwrap()
    }

    #[cfg(test)]
    pub fn count(&self, topic: &str) -> i64 {
        self.conn
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE topic=?1",
                params![topic],
                |r| r.get(0),
            )
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    #[test]
    fn seq_monotonic_per_topic() {
        let s = store();
        assert_eq!(s.publish_simple("a", "1", &[]).message.seq, 1);
        assert_eq!(s.publish_simple("a", "2", &[]).message.seq, 2);
        assert_eq!(s.publish_simple("b", "1", &[]).message.seq, 1);
    }

    #[test]
    fn since_returns_raw_ordered() {
        let s = store();
        for i in 0..5 {
            s.publish_simple("t", &format!("m{i}"), &[]);
        }
        let p = s.since("t", 2, 200).unwrap();
        assert_eq!(p.messages.len(), 3);
        assert_eq!(p.last_scanned, 5);
        let first: Message = serde_json::from_str(&p.messages[0]).unwrap();
        assert_eq!(first.seq, 3);
        let p = s.since("t", 5, 200).unwrap();
        assert!(p.messages.is_empty());
        assert_eq!(p.last_scanned, 5);
    }

    #[test]
    fn ack_floor_is_min_over_live_clients() {
        let s = store();
        s.ack("t", "c1", 5).unwrap();
        s.ack("t", "c2", 3).unwrap();
        s.ack("t", "c1", 7).unwrap();
        let floor: i64 = s
            .conn
            .lock()
            .query_row("SELECT global_acked FROM topics WHERE topic='t'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(floor, 3); // c2 lags
    }

    #[test]
    fn stale_cursor_releases_ack_floor() {
        let s = store();
        s.ack("t", "c1", 5).unwrap();
        s.ack("t", "c2", 3).unwrap();
        assert_eq!(s.global_acked("t"), 3);
        // c2 dies: age its cursor past the liveness window
        s.conn
            .lock()
            .execute(
                "UPDATE client_cursors SET updated_at = 1 WHERE cid='c2'",
                [],
            )
            .unwrap();
        // the next ack from anyone re-ages the floor over live cursors only
        s.ack("t", "c1", 8).unwrap();
        assert_eq!(s.global_acked("t"), 8); // c2 no longer pins it
        // and prune (which re-ages too) now releases acked-before-8 rows
        s.conn
            .lock()
            .execute("UPDATE messages SET ts = 0", [])
            .unwrap();
        s.prune(5000, 60).unwrap();
        assert_eq!(s.count("t"), 0);
    }

    #[test]
    fn prune_keeps_unacked_and_caps() {
        let s = store();
        for i in 0..10 {
            s.publish_simple("t", &format!("m{i}"), &[]);
        }
        s.ack("t", "c1", 10).unwrap();
        // min retention 60s: everything is younger -> nothing pruned
        s.prune(5000, 60).unwrap();
        assert_eq!(s.count("t"), 10);
        // cap at 4 keeps newest 4
        s.prune(4, 0).unwrap();
        assert_eq!(s.count("t"), 4);
        let p = s.since("t", 0, 200).unwrap();
        let first: Message = serde_json::from_str(&p.messages[0]).unwrap();
        assert_eq!(first.seq, 7);
    }

    #[test]
    fn prune_drops_expired() {
        let s = store();
        let mut p = Publish {
            title: "short".into(),
            body: String::new(),
            tags: vec![],
            priority: Default::default(),
            category: String::new(),
            icon: String::new(),
            silent: false,
            ttl: 1,
            timeout_ms: 0,
            dedup_key: String::new(),
            channel: String::new(),
        };
        s.publish("t", &p).unwrap();
        p.ttl = 86_400;
        s.publish("t", &p).unwrap();
        // force expiry
        s.conn
            .lock()
            .execute("UPDATE messages SET expires_at = 1 WHERE seq = 1", [])
            .unwrap();
        s.prune(5000, 0).unwrap();
        assert_eq!(s.count("t"), 1);
    }
}
