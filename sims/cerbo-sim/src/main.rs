//! `aetherrv-sim-cerbo [--port N] [--tls] [--config FILE.json] [--no-apply] [--no-reads] [--log FILE] [--port-file FILE]`
//!
//! A simulated Victron Cerbo GX: starts a Mosquitto of its own on `127.0.0.1:<port>` (a free port when not given;
//! 1883 and 8883 are refused, and an occupied port is an error, never a connection to whatever listens there),
//! then answers `R/` and applies `W/` like Venus OS. Point `aetherrv-victron` (or the Python adapter) at
//! `host = "127.0.0.1"`, `port = <port>`.
//!
//! `--config` takes a JSON object (see `aetherrv_sim::cerbo::CerboConfig::from_json`), e.g.
//! `{"values": {"vebus/276/Mode": 4}, "apply_writes": false, "auth": {"user": "u", "password": "p"}}`.
//! Credentials go in that file, never on the command line.
//!
//! Prints `cerbo-sim: listening on 127.0.0.1:<port> portal <id>` once ready; every `R/` and `W/` goes to `--log`
//! (JSON lines). SIGHUP puts the model back to its configured values (for back-to-back runs of two clients).
//! Runs until SIGTERM / Ctrl-C.

use aetherrv_sim::cerbo::{BrokerOptions, Cerbo, CerboConfig, PrivateBroker};
use serde_json::Value;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let mut bo = BrokerOptions::default();
    let mut cfg = CerboConfig::van();
    let mut port_file = None;
    let (mut no_apply, mut no_reads, mut log) = (false, false, None);
    while let Some(k) = a.next() {
        match k.as_str() {
            "--port" => bo.port = a.next().unwrap_or_default().parse()?,
            "--tls" => bo.tls = true,
            "--no-apply" => no_apply = true,
            "--no-reads" => no_reads = true,
            "--log" => log = a.next().map(std::path::PathBuf::from),
            "--port-file" => port_file = a.next(),
            "--config" => {
                let p = a.next().unwrap_or_default();
                let j: Value = serde_json::from_str(&std::fs::read_to_string(&p)?)?;
                cfg = CerboConfig::from_json(&j)?;
                if let Some(au) = j.get("auth") {
                    let s = |k: &str| au.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    bo.auth = Some((s("user"), s("password")));
                }
            }
            _ => anyhow::bail!("unknown option {k}"),
        }
    }
    if no_apply {
        cfg.apply_writes = false;
    }
    if no_reads {
        cfg.answer_reads = false;
    }
    if log.is_some() {
        cfg.log = log;
    }
    let broker = PrivateBroker::start(bo)?;
    let cerbo = Cerbo::start(cfg, &broker).await?;
    if let Some(p) = port_file {
        std::fs::write(p, broker.port.to_string())?;
    }
    println!("cerbo-sim: listening on 127.0.0.1:{}{} portal {}", broker.port, if broker.tls { " (TLS)" } else { "" }, cerbo.portal());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = hup.recv() => {
                cerbo.reset();
                println!("cerbo-sim: model reset");
            }
        }
    }
    drop(cerbo);
    drop(broker);
    Ok(())
}
