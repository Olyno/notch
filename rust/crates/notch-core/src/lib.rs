//! Wire model shared by the notch relay, MCP server and (mirrored in Dart)
//! the client SDK. Keep this file in sync with `libs/dart/lib/src/model.dart`.

use serde::{Deserialize, Serialize};

/// Priority maps to Android `NotificationManager` importance on the client:
/// min/low -> background/low channel, default/high/max -> high/importance
/// with sound+heads-up. `silent` bypasses alerting entirely regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Min,
    Low,
    #[default]
    Default,
    High,
    Max,
}

/// A notification as it travels on the wire (publish response, WS frame,
/// HTTP poll). Relay-assigned fields (`seq`, `id`, `ts`) are filled at
/// publish time; clients MUST treat them as read-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub topic: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Free-form classification labels; the only field filters run on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default_priority")]
    pub priority: Priority,
    /// Android `Notification.Category` hint, e.g. `msg`, `call`, `event`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// Deliver but never alert (no sound/vibration/heads-up).
    #[serde(default, skip_serializing_if = "is_false")]
    pub silent: bool,
    /// Relay retention hint in seconds; 0/absent -> default.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub ttl: u32,
    /// Client auto-dismiss in ms; 0/absent -> sticky until tapped.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub timeout_ms: u64,
    /// Same key replaces the previous notification instead of stacking.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dedup_key: String,
    /// Android channel override; empty -> channel derived from topic.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    /// Deep link opened when the user taps the notification (http/https or a
    /// custom scheme handled by the sender's app). Empty -> tapping opens the
    /// Notch app itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default)]
    pub ts: i64,
}

/// What a publisher (agent via MCP, curl via HTTP) sends. Same shape as
/// [`Message`] minus relay-owned fields.
#[derive(Debug, Clone, Deserialize)]
pub struct Publish {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub silent: bool,
    #[serde(default)]
    pub ttl: u32,
    #[serde(default)]
    pub timeout_ms: u64,
    #[serde(default)]
    pub dedup_key: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub url: String,
}

impl Publish {
    pub fn into_message(self, topic: String, seq: u64, ts: i64) -> Message {
        let id = short_id(&topic, seq, &self.title, &self.body);
        Message {
            seq,
            id,
            topic,
            title: self.title,
            body: self.body,
            tags: self.tags,
            priority: self.priority,
            category: self.category,
            icon: self.icon,
            silent: self.silent,
            ttl: self.ttl,
            timeout_ms: self.timeout_ms,
            dedup_key: self.dedup_key,
            channel: self.channel,
            url: self.url,
            ts,
        }
    }
}

/// Topic ids: [A-Za-z0-9_-]{1,64}; cheap to validate, safe in URLs/SQLite PKs.
pub fn valid_topic(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Batch frame shared by WS replay and the HTTP poll response: clients
/// parse exactly one shape for "one or more messages". Live single
/// messages are sent bare (smallest possible payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub messages: Vec<Message>,
}

/// Server-side tag predicate: `all` every tag present, `any` at least one,
/// `none` none present. Empty predicates are inert.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub all: Vec<String>,
    pub any: Vec<String>,
    pub none: Vec<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.all.is_empty() && self.any.is_empty() && self.none.is_empty()
    }

    pub fn matches(&self, tags: &[String]) -> bool {
        self.all.iter().all(|t| tags.iter().any(|x| x == t))
            && (self.any.is_empty() || self.any.iter().any(|t| tags.iter().any(|x| x == t)))
            && !self.none.iter().any(|t| tags.iter().any(|x| x == t))
    }

    /// Parse from query-string pairs `all=a,b any=join none=leave`.
    pub fn from_query(get: impl Fn(&str) -> Option<String>) -> Self {
        let split = |s: String| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect()
        };
        Filter {
            all: get("all").map(&split).unwrap_or_default(),
            any: get("any").map(&split).unwrap_or_default(),
            none: get("none").map(&split).unwrap_or_default(),
        }
    }
}

/// Deterministic short id for client-side dedup across reconnects where the
/// cursor may not have persisted: topic+seq is already unique per relay, the
/// fnv mix guards against a re-created store reusing seq numbers.
pub fn short_id(topic: &str, seq: u64, title: &str, body: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [topic, &seq.to_string(), title, body] {
        for b in part.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

fn is_false(v: &bool) -> bool {
    !*v
}
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}
fn is_default_priority(p: &Priority) -> bool {
    *p == Priority::Default
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(tags: &[&str]) -> Message {
        Publish {
            title: "t".into(),
            body: "b".into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            priority: Priority::default(),
            category: String::new(),
            icon: String::new(),
            silent: false,
            ttl: 0,
            timeout_ms: 0,
            dedup_key: String::new(),
            channel: String::new(),
            url: String::new(),
        }
        .into_message("x".into(), 1, 0)
    }

    #[test]
    fn wire_is_minimal() {
        let m = msg(&[]);
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(
            j,
            format!(
                r#"{{"seq":1,"id":"{}","topic":"x","title":"t","body":"b","ts":0}}"#,
                short_id("x", 1, "t", "b")
            )
        );
    }

    #[test]
    fn unknown_fields_ignored() {
        let json = r#"{"seq":2,"topic":"a","title":"t","future_field":42,"body":"b","ts":1}"#;
        let m: Message = serde_json::from_str(json).unwrap();
        assert_eq!(m.seq, 2);
    }

    #[test]
    fn filter_semantics() {
        let f = Filter {
            all: vec![],
            any: vec!["join".into()],
            none: vec!["leave".into()],
        };
        assert!(f.matches(&["join".into(), "friend".into()]));
        assert!(!f.matches(&[("leave".into())])); // none wins over any
        assert!(!f.matches(&["other".into()])); // any not satisfied
        assert!(Filter::default().matches(&[])); // empty filter is inert

        let f2 = Filter {
            all: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert!(f2.matches(&["b".into(), "a".into(), "c".into()]));
        assert!(!f2.matches(&["a".into()]));
    }
    #[test]
    fn batch_roundtrip() {
        let m = msg(&["join"]);
        let bare = serde_json::to_string(&m).unwrap();
        assert!(bare.starts_with(r#"{"seq"#));
        let batch = serde_json::to_string(&Batch {
            messages: vec![m.clone()],
        })
        .unwrap();
        assert!(batch.starts_with(r#"{"messages":"#));
        let back: Batch = serde_json::from_str(&batch).unwrap();
        assert_eq!(back.messages, vec![m]);
    }

    #[test]
    fn priority_lowercase() {
        assert_eq!(serde_json::to_string(&Priority::High).unwrap(), "\"high\"");
    }
}
