//! Raw HTTP/1.1 client for the notch relay — no hyper/reqwest, just tokio.
//! One short-lived connection per request (Connection: close): the relay is
//! localhost/LAN, and connection pooling would buy nothing but code.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone)]
pub struct Relay {
    pub host: String,
    pub port: u16,
}

const TIMEOUT: Duration = Duration::from_secs(10);

impl Relay {
    /// Parse `http://host[:port]` (https rejected: TLS belongs to a reverse
    /// proxy in this design; see docs/PROTOCOL.md).
    pub fn parse(url: &str) -> Result<Relay, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("notch-mcp speaks plain http to the relay; got '{url}'"))?;
        let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !path.is_empty() {
            return Err(format!(
                "relay url must be a bare origin, got path '/{path}'"
            ));
        }
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| "bad port")?),
            None => (hostport.to_string(), 80),
        };
        if host.is_empty() {
            return Err("empty host".into());
        }
        Ok(Relay { host, port })
    }

    /// Issue one request; returns the response body on 2xx.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, String> {
        let req = match body {
            Some(b) => format!(
                "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                self.host,
                b.len()
            ),
            None => format!(
                "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                self.host
            ),
        };
        let fut = async {
            let mut sock = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(|e| format!("relay unreachable: {e}"))?;
            sock.write_all(req.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            let mut raw = Vec::with_capacity(4096);
            sock.read_to_end(&mut raw)
                .await
                .map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&raw).into_owned();
            let (head, resp_body) = text
                .split_once("\r\n\r\n")
                .ok_or("malformed relay response")?;
            let status: u16 = head
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .ok_or("malformed status line")?;
            if !(200..300).contains(&status) {
                return Err(format!("relay returned {status}: {}", resp_body.trim()));
            }
            Ok(resp_body.to_string())
        };
        tokio::time::timeout(TIMEOUT, fut)
            .await
            .map_err(|_| "relay request timed out")?
    }

    pub async fn publish(&self, topic: &str, json: &str) -> Result<String, String> {
        self.request("POST", &format!("/t/{topic}"), Some(json))
            .await
    }

    pub async fn poll(&self, path: &str) -> Result<String, String> {
        self.request("GET", path, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_forms() {
        assert!(matches!(
            Relay::parse("http://localhost"),
            Ok(Relay { port: 80, .. })
        ));
        let r = Relay::parse("http://192.168.1.5:8899").unwrap();
        assert_eq!((r.host.as_str(), r.port), ("192.168.1.5", 8899));
        assert!(Relay::parse("https://x").is_err());
        assert!(Relay::parse("http://x/prefix").is_err());
        assert!(Relay::parse("http://x:99999").is_err());
    }

    #[tokio::test]
    async fn request_roundtrip() {
        // in-process fake relay
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            assert!(req.starts_with("POST /t/x HTTP/1.1"), "{req}");
            assert!(req.contains("Content-Length: 9"));
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n{\"seq\":7}",
                )
                .await;
        });
        let relay = Relay {
            host: addr.ip().to_string(),
            port: addr.port(),
        };
        assert_eq!(
            relay.publish("x", r#"{"a":"b"}"#).await.unwrap(),
            "{\"seq\":7}"
        );
    }

    #[tokio::test]
    async fn non_2xx_is_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock
                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 5\r\nConnection: close\r\n\r\nnope1")
                .await;
        });
        let relay = Relay {
            host: addr.ip().to_string(),
            port: addr.port(),
        };
        let err = relay.publish("x", "{}").await.unwrap_err();
        assert!(err.contains("400"), "{err}");
    }
}
