//! `command_log` writer: every command attempt (who, what, outcome) for one device.
//!
//! The table is the Python one, in the same SQLite file (`history.sqlite3`, WAL), so the rollback needs no data
//! conversion: `command_log (t REAL, device TEXT, cmd TEXT, value TEXT, outcome TEXT, detail TEXT, client TEXT)`.
//! Writes go to a worker thread through a bounded channel: a lock held by another process never stalls the
//! caller's control loop, and with the database unusable the row still reaches the journal (logged first).

use crate::pyfmt::py_str;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

pub const COMMAND_LOG_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS command_log (t REAL, device TEXT, cmd TEXT, \
     value TEXT, outcome TEXT, detail TEXT, client TEXT)";

/// One row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub t: f64,
    pub device: String,
    pub cmd: String,
    pub value: String,
    pub outcome: String,
    pub detail: String,
    pub client: String,
}

/// Open a connection the way every AetherRV writer does: WAL, NORMAL sync, a busy timeout.
pub fn open_db(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let db = rusqlite::Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(db)
}

enum Msg {
    Row(Row),
    Sync(std::sync::mpsc::Sender<()>),
}

#[derive(Clone)]
pub struct CommandLog {
    device: String,
    tx: Option<SyncSender<Msg>>,
    /// In-memory copy of the rows, for tests and diagnostics (bounded).
    mem: std::sync::Arc<std::sync::Mutex<Vec<Row>>>,
}

impl CommandLog {
    /// A log for `device` writing to `db_path`; `None` → journal + memory only (tests).
    pub fn new(device: &str, db_path: Option<PathBuf>) -> Self {
        let mem = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tx = db_path.map(|p| {
            let (tx, rx) = sync_channel::<Msg>(1024);
            std::thread::Builder::new()
                .name(format!("cmdlog-{device}"))
                .spawn(move || writer(p, rx))
                .expect("spawn command log writer");
            tx
        });
        CommandLog { device: device.to_string(), tx, mem }
    }

    pub fn memory(device: &str) -> Self {
        Self::new(device, None)
    }

    /// Log one attempt (Python `_log(cmd, value, outcome, detail, client)`): a WARNING line, then the row.
    pub fn log(&self, cmd: &str, value: &Value, outcome: &str, detail: &str, client: &str) {
        let v = py_str(value);
        tracing::warn!("{} command {cmd}={v} from {client}: {outcome} {detail}", self.device);
        let row = Row {
            t: crate::clock::now_s(),
            device: self.device.clone(),
            cmd: cmd.to_string(),
            value: v,
            outcome: outcome.to_string(),
            detail: detail.to_string(),
            client: client.to_string(),
        };
        {
            let mut m = self.mem.lock().unwrap();
            if m.len() >= 1000 {
                m.remove(0);
            }
            m.push(row.clone());
        }
        if let Some(tx) = &self.tx {
            match tx.try_send(Msg::Row(row)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => tracing::error!("command log queue full; row kept in the journal only"),
                Err(TrySendError::Disconnected(_)) => tracing::error!("command log writer gone; row in the journal only"),
            }
        }
    }

    /// Rows logged by this process (newest last).
    pub fn rows(&self) -> Vec<Row> {
        self.mem.lock().unwrap().clone()
    }

    /// Wait until every queued row has been written (tests, shutdown).
    pub fn flush(&self) {
        if let Some(tx) = &self.tx {
            let (stx, srx) = std::sync::mpsc::channel();
            if tx.send(Msg::Sync(stx)).is_ok() {
                let _ = srx.recv_timeout(std::time::Duration::from_secs(10));
            }
        }
    }

    pub fn device(&self) -> &str {
        &self.device
    }
}

fn writer(path: PathBuf, rx: std::sync::mpsc::Receiver<Msg>) {
    let mut db: Option<rusqlite::Connection> = None;
    for msg in rx {
        let row = match msg {
            Msg::Row(r) => r,
            Msg::Sync(done) => {
                let _ = done.send(());
                continue;
            }
        };
        if db.is_none() {
            match open_db(&path).and_then(|d| d.execute(COMMAND_LOG_SCHEMA, []).map(|_| d)) {
                Ok(d) => db = Some(d),
                Err(e) => {
                    tracing::error!("command log {}: {e}", path.display());
                    continue;
                }
            }
        }
        let r = db.as_ref().unwrap().execute(
            "INSERT INTO command_log VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![row.t, row.device, row.cmd, row.value, row.outcome, row.detail, row.client],
        );
        if let Err(e) = r {
            tracing::error!("command log write failed: {e}");
            db = None; // reopen next time
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_python_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("history.sqlite3");
        let l = CommandLog::new("espar", Some(p.clone()));
        l.log("set", &json!({"mode": "off"}), "noop", "already off (not driving the heater)", "kiosk");
        l.flush();
        let db = rusqlite::Connection::open(&p).unwrap();
        let (dev, val, out, client): (String, String, String, String) = db
            .query_row("SELECT device, value, outcome, client FROM command_log", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!((dev.as_str(), val.as_str(), out.as_str(), client.as_str()), ("espar", "{'mode': 'off'}", "noop", "kiosk"));
        let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
    }
}
