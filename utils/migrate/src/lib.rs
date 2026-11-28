//! `aetherrv-migrate`: checks (and where safe, fixes) the files the Python system leaves, so the Rust system uses
//! them as they are (ARCHITECTURE.md §9): the config, the secrets file, the history database, settings.json and the
//! radar cache. The Rust system keeps every format and schema, so a rollback needs no conversion back; this tool
//! never rewrites the config or the secrets.

use aetherrv_core::config::Cfg;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Note,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub level: Level,
    pub what: String,
}

fn f(level: Level, what: impl Into<String>) -> Finding {
    Finding { level, what: what.into() }
}

pub const KINDS: [&str; 11] = ["espar", "n2k", "victron", "rixens_http", "rixens", "capture", "epoch_ble", "ruuvi", "starlink", "peplink", "demo"];

/// The config contract (`/etc/aetherrv/aetherrv.toml`).
pub fn check_config(text: &str) -> Vec<Finding> {
    let mut out = vec![];
    let cfg = match Cfg::parse(text) {
        Ok(c) => c,
        Err(e) => return vec![f(Level::Error, format!("config: not valid TOML: {e}"))],
    };
    let buses: Vec<String> = cfg.at("bus").entries().into_iter().map(|(n, _)| n).collect();
    for (name, d) in cfg.at("device").entries() {
        let kind = d.str("kind", "");
        if !KINDS.contains(&kind.as_str()) {
            out.push(f(Level::Warn, format!("device.{name}: unknown kind {kind:?} (ignored, as by Python)")));
        }
        for flag in ["tx_enabled", "diag_tx"] {
            if let Some(v) = d.get(flag)
                && !v.is_boolean()
            {
                out.push(f(Level::Warn, format!("device.{name}.{flag} = {v} is not a boolean: the Rust system treats it as OFF (D5)")));
            }
        }
        if kind == "demo" && d.bool("enabled", false) {
            out.push(f(Level::Warn, format!("device.{name}: demo mode is enabled, but it was dropped (Q-D1): no synthetic data will appear")));
        }
        if let Some(b) = d.opt_str("bus").filter(|b| !buses.contains(b)) {
            out.push(f(Level::Warn, format!("device.{name}: bus {b:?} is not configured (skipped, as by Python)")));
        }
        if let Some(b) = d.opt_str("backup_bus").filter(|b| !buses.contains(b)) {
            out.push(f(Level::Note, format!("device.{name}: backup_bus {b:?} is not configured (no failover)")));
        }
        if d.flag("tx_enabled") {
            out.push(f(Level::Note, format!("device.{name}: transmit enabled")));
        }
    }
    if cfg.at("power").bool("auto_shutdown", false) {
        out.push(f(Level::Note, "power.auto_shutdown on: heat lockout enforced by the guard and inside the heater services"));
    }
    let web = cfg.at("web");
    if web.has("static_dir") && !web.has("ui_dir") {
        out.push(f(Level::Note, "web.static_dir is the Python UI's; the Rust gateway serves [web] ui_dir (default /opt/aetherrv-rs/ui)"));
    }
    if out.iter().all(|x| x.level < Level::Warn) {
        out.push(f(Level::Ok, format!("config: {} devices, {} buses", cfg.at("device").entries().len(), buses.len())));
    }
    out
}

/// The secrets file: present, root:aetherrv 0640, known keys (values are never read into the report).
pub fn check_secrets(path: &Path, check_mode: bool) -> Vec<Finding> {
    let mut out = vec![];
    let Ok(text) = std::fs::read_to_string(path) else { return vec![f(Level::Note, format!("{}: not readable (none configured?)", path.display()))] };
    let known = ["victron_mqtt_user", "victron_mqtt_password", "peplink_client_id", "peplink_client_secret", "peplink_ro_user",
                 "peplink_ro_password", "peplink_rw_user", "peplink_rw_password", "peplink_rw_client_id", "peplink_rw_client_secret", "tomtom_api_key"];
    match Cfg::parse(&text) {
        Ok(c) => {
            for (k, _) in c.entries() {
                if !known.contains(&k.as_str()) {
                    out.push(f(Level::Note, format!("secrets: unknown key {k} (unused)")));
                }
            }
        }
        Err(e) => out.push(f(Level::Error, format!("secrets: not valid TOML: {e}"))),
    }
    if check_mode {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata(path)
            && m.mode() & 0o007 != 0
        {
            out.push(f(Level::Warn, format!("secrets: mode {:o} is world-accessible (want 0640)", m.mode() & 0o777)));
        }
    }
    if out.iter().all(|x| x.level < Level::Warn) {
        out.push(f(Level::Ok, "secrets: ok"));
    }
    out
}

/// The history database: integrity, the four tables, WAL. `fix` creates missing tables (as the services would).
pub fn check_history(path: &Path, fix: bool) -> Vec<Finding> {
    let mut out = vec![];
    let db = match rusqlite::Connection::open_with_flags(
        path,
        if fix { rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE } else { rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY },
    ) {
        Ok(d) => d,
        Err(e) => return vec![f(Level::Error, format!("history {}: {e}", path.display()))],
    };
    let ok: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0)).unwrap_or_else(|e| e.to_string());
    if ok != "ok" {
        out.push(f(Level::Error, format!("history: quick_check: {ok}")));
    }
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .and_then(|mut s| s.query_map([], |r| r.get(0)).map(|it| it.filter_map(Result::ok).collect()))
        .unwrap_or_default();
    for (t, ddl) in [
        ("samples", "CREATE TABLE IF NOT EXISTS samples (key TEXT NOT NULL, t REAL NOT NULL, v REAL NOT NULL)"),
        ("alert_log", "CREATE TABLE IF NOT EXISTS alert_log (t REAL, event TEXT, id TEXT, severity TEXT, title TEXT, detail TEXT)"),
        ("command_log", aetherrv_core::cmdlog::COMMAND_LOG_SCHEMA),
        ("counters", "CREATE TABLE IF NOT EXISTS counters (key TEXT PRIMARY KEY, day TEXT, value REAL)"),
    ] {
        if !tables.contains(&t.to_string()) {
            if fix {
                match db.execute(ddl, []) {
                    Ok(_) => out.push(f(Level::Note, format!("history: created table {t}"))),
                    Err(e) => out.push(f(Level::Error, format!("history: create {t}: {e}"))),
                }
            } else {
                out.push(f(Level::Note, format!("history: no {t} table yet (the service creates it)")));
            }
        }
    }
    let n: i64 = db.query_row("SELECT count(*) FROM samples", [], |r| r.get(0)).unwrap_or(-1);
    let mode: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap_or_default();
    if mode != "wal" {
        out.push(f(Level::Note, format!("history: journal_mode {mode} (the services switch it to WAL)")));
    }
    out.push(f(Level::Ok, format!("history: {n} samples, tables {tables:?}")));
    out
}

/// settings.json: the Python prefs (unknown keys are dropped by both systems).
pub fn check_settings(path: &Path) -> Vec<Finding> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![f(Level::Note, "settings.json: none (defaults)")] };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&text) else { return vec![f(Level::Warn, "settings.json: not a JSON object (defaults will be used)")] };
    let mut out = vec![];
    for (k, v) in &m {
        let ok = match k.as_str() {
            "screen_off_min" => v.as_i64().is_some_and(|x| (0..=240).contains(&x)),
            "auto_timezone" | "fridge_off" | "freezer_off" => v.is_boolean(),
            _ => {
                out.push(f(Level::Note, format!("settings.json: unknown key {k} (ignored)")));
                true
            }
        };
        if !ok {
            out.push(f(Level::Warn, format!("settings.json: {k} = {v} is out of range (the default is used)")));
        }
    }
    out.push(f(Level::Ok, format!("settings.json: {} keys", m.len())));
    out
}

/// The radar cache: `<t>/<z>/<x>/<y>.png`, `done` markers, `flow.json`.
pub fn check_radar(dir: &Path) -> Vec<Finding> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![f(Level::Note, format!("radar cache {}: none", dir.display()))] };
    let (mut frames, mut done, mut flows, mut odd) = (0, 0, 0, 0);
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.parse::<u64>().is_ok() && e.path().is_dir() {
            frames += 1;
            done += e.path().join("done").exists() as i32;
            flows += e.path().join("flow.json").exists() as i32;
        } else {
            odd += 1;
        }
    }
    let mut out = vec![f(Level::Ok, format!("radar cache: {frames} frames ({done} complete, {flows} motion fields)"))];
    if odd > 0 {
        out.push(f(Level::Note, format!("radar cache: {odd} other entries (ignored)")));
    }
    out
}
