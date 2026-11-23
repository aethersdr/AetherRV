//! Client for the root admin helper (`aetherrv-admind`, `/run/aetherrv-admin.sock`): one JSON request per
//! connection, `{"verb": str, "args": {...}}` → one JSON reply line. The helper enforces its own verb list.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const SOCK: &str = "/run/aetherrv-admin.sock";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct AdminError(pub String);

#[derive(Clone, Debug)]
pub struct AdminClient {
    pub path: PathBuf,
}

impl Default for AdminClient {
    fn default() -> Self {
        AdminClient { path: SOCK.into() }
    }
}

impl AdminClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        AdminClient { path: path.into() }
    }

    pub async fn call(&self, verb: &str, args: Value, timeout: Duration) -> Result<Value, AdminError> {
        let conn = tokio::time::timeout(Duration::from_secs(5), tokio::net::UnixStream::connect(&self.path)).await;
        let mut s = match conn {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(AdminError(format!("admin helper unavailable ({})", kind(&e)))),
            Err(_) => return Err(AdminError("admin helper unavailable (TimeoutError)".into())),
        };
        let args = if args.is_null() { json!({}) } else { args };
        let mut line = serde_json::to_vec(&json!({"verb": verb, "args": args})).unwrap();
        line.push(b'\n');
        let io = async {
            s.write_all(&line).await?;
            let mut r = BufReader::new(s);
            let mut out = String::new();
            r.read_line(&mut out).await?;
            Ok::<String, std::io::Error>(out)
        };
        match tokio::time::timeout(timeout, io).await {
            Ok(Ok(out)) => {
                if out.trim().is_empty() {
                    Ok(json!({}))
                } else {
                    serde_json::from_str(&out).map_err(|_| AdminError("admin helper error (ValueError)".into()))
                }
            }
            Ok(Err(e)) => Err(AdminError(format!("admin helper error ({})", kind(&e)))),
            Err(_) => Err(AdminError("admin helper error (TimeoutError)".into())),
        }
    }
}

fn kind(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
        _ => "OSError",
    }
}
