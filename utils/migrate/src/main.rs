//! `aetherrv-migrate [--fix] [--root <dir>]`: check the Python system's files before the cutover (docs/CUTOVER.md).
//! `--root` points at a copy (e.g. harness/out/van-files) instead of /etc/aetherrv and /var/lib/aetherrv.

use aetherrv_migrate::*;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let fix = args.iter().any(|a| a == "--fix");
    let root = args.iter().position(|a| a == "--root").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let (etc, var) = match &root {
        Some(r) => (r.clone(), r.clone()),
        None => (PathBuf::from("/etc/aetherrv"), PathBuf::from("/var/lib/aetherrv")),
    };
    let mut all = vec![];
    let cfg_text = std::fs::read_to_string(etc.join("aetherrv.toml")).unwrap_or_default();
    all.extend(check_config(&cfg_text));
    let sec = if root.is_some() { etc.join("secrets-shape.toml") } else { etc.join("secrets.toml") };
    all.extend(check_secrets(&sec, root.is_none()));
    all.extend(check_history(&var.join("history.sqlite3"), fix));
    all.extend(check_settings(&var.join("settings.json")));
    all.extend(check_radar(&var.join("radar")));
    let worst = all.iter().map(|x| x.level.clone()).max().unwrap_or(Level::Ok);
    for x in &all {
        println!("{:<5} {}", format!("{:?}", x.level).to_uppercase(), x.what);
    }
    std::process::exit(match worst {
        Level::Error => 2,
        Level::Warn => 1,
        _ => 0,
    });
}
