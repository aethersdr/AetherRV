//! The migration checks on synthetic files, and on copies of the van's real files when they're present
//! (harness/out/van-files, private, gitignored: `tools/van-read.sh config|secrets-shape|history-copy`).

use aetherrv_migrate::*;

fn levels(v: &[Finding]) -> Vec<Level> {
    v.iter().map(|f| f.level.clone()).collect()
}

#[test]
fn config_warns_on_non_boolean_tx_flags_and_demo() {
    let r = check_config("[device.espar]\nkind = 'espar'\ntx_enabled = 'yes'\nbus = 'hat1'\n[device.demo]\nkind = 'demo'\nenabled = true\n");
    let w: Vec<&str> = r.iter().filter(|f| f.level == Level::Warn).map(|f| f.what.as_str()).collect();
    assert!(w.iter().any(|x| x.contains("tx_enabled") && x.contains("OFF")), "{w:?}");
    assert!(w.iter().any(|x| x.contains("demo mode")), "{w:?}");
    assert!(w.iter().any(|x| x.contains("bus \"hat1\" is not configured")), "{w:?}");
    assert!(levels(&check_config("not = [toml")).contains(&Level::Error));
}

#[test]
fn settings_and_history_checks() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("s.json"), r#"{"screen_off_min": 999, "auto_timezone": true, "x": 1}"#).unwrap();
    let r = check_settings(&d.path().join("s.json"));
    assert!(r.iter().any(|f| f.level == Level::Warn && f.what.contains("screen_off_min")));
    let db = d.path().join("h.sqlite3");
    rusqlite::Connection::open(&db).unwrap().execute("CREATE TABLE samples (key TEXT NOT NULL, t REAL NOT NULL, v REAL NOT NULL)", []).unwrap();
    let r = check_history(&db, true);
    assert!(r.iter().any(|f| f.what.contains("created table command_log")), "{r:?}");
    let r = check_history(&db, false);
    assert!(!levels(&r).contains(&Level::Error));
}

/// The real files from the van (skipped when the private copies aren't on this machine).
#[test]
fn real_van_files_pass() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness/out/van-files");
    if !root.join("aetherrv.toml").exists() {
        return eprintln!("no van copies: skipped");
    }
    let mut all = check_config(&std::fs::read_to_string(root.join("aetherrv.toml")).unwrap());
    all.extend(check_secrets(&root.join("secrets-shape.toml"), false));
    let tmp = tempfile::tempdir().unwrap();
    for f in ["history.sqlite3", "history.sqlite3-wal", "history.sqlite3-shm"] {
        let _ = std::fs::copy(root.join(f), tmp.path().join(f));
    }
    all.extend(check_history(&tmp.path().join("history.sqlite3"), false));
    all.extend(check_settings(&root.join("settings.json")));
    let worst = all.iter().map(|f| f.level.clone()).max().unwrap();
    assert!(worst < Level::Warn, "{all:?}");
}
