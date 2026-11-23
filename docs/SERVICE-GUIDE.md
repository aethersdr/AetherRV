# Writing an AetherRV service (Rust)

How every service in `services/` is built, so each one looks and fails like the others. The design is in
`docs/ARCHITECTURE.md`; the reference service is `services/espar`.

## Shape

```
services/<name>/
  Cargo.toml        package aetherrv-<name>, [[bin]] aetherrv-<name>, deps from [workspace.dependencies], [lints] workspace = true
  src/main.rs       Args::parse → logging::init → Cfg::load → tokio runtime → <crate>::run(cfg, opts)
  src/lib.rs        the service: pure logic + run(cfg, opts)
  tests/*.rs        the ported Python tests (/// py: <pytest id>), plus process-level tests when it owns hardware
```

When you create a new crate directory, write `src/lib.rs` (even empty) **before** `Cargo.toml`: the workspace globs
`services/*`, and a manifest without sources stops every other crate from building.

## Rules

- **The Python module is the specification.** Same rules, timings, state keys (names, JSON types, order), detail
  texts, command-log rows (`aetherrv_core::cmdlog::CommandLog`, which writes the Python `str(value)` form) and log
  messages. Where you can't be identical, say so in the report (and in `ARCHITECTURE.md` §13a once accepted).
- **Logic is testable without I/O.** Time comes in as `now: f64` (Unix seconds, Python `time.time()`) or
  `CLOCK_BOOTTIME` where a loop must survive wall-clock jumps. State reads go through
  `aetherrv_core::state::StateView` (`StateExt` gives `num`, `is_true`, `list`, …), so tests use a `StateStore` and
  the service uses the bus's `RemoteState` (or `Merged { local, remote }`).
- **Publishing**: `bus.state().set(key, json!(…))` / `update([...])`. Never block a device loop on the bus.
- **Reading other services**: `bus.subscribe_state("<owner>", "<key>")` for each key you need (the owner is the
  service that publishes it, ARCHITECTURE.md §2/§4.5). A key of an owner that is down reads as `None`.
- **Commands**: implement `aetherrv_core::command::CommandHandler` and `bus.serve("<device>", handler, Some(log))`.
  Port `command()` exactly: TxGate check (`TxGate::from_config(&cfg).only(&["<device>"])`), fresh read from the
  device itself (never bus state), send only if different, send once, verify, never retry, log every attempt.
  Return `Reply::ok/refused/unverified` per the outcome mapping in `aetherrv_core::command::Outcome::result`.
  Heater OFFs and dimmer releases are exempt from the TTL (`ttl_exempt`).
- **Safety-relevant value parsing**: an on/off value must be a JSON boolean, a number a JSON number. Refuse
  anything else ("bad request"); don't reproduce Python's `bool("false") == True`.
- **Secrets** come from `aetherrv_core::config::load_secrets(&cfg)`; never log, publish or put them on a command
  line.
- **Round like Python**: `aetherrv_core::num::round(x, n)` / `round_int`.
- **systemd**: `aetherrv_core::notify::ready()` once serving; `spawn_runtime_watchdog()` unless the service feeds
  the watchdog from its own loop (Espar).
- **Simulators** for your device go in `crates/sim` (library, used in tests) plus `sims/<name>-sim` (a plain
  process), wire-level (HTTP, MQTT, gRPC, UDP), and must refuse to point at anything but loopback/vcan.

## Testing

- `CARGO_TARGET_DIR=target/<you> cargo test -p aetherrv-<name>` (a private target dir avoids lock contention with
  parallel work). `cargo clippy -p aetherrv-<name> --all-targets` clean.
- A private broker for tests: `aetherrv_core::testutil::Broker::start(false)` (feature `testutil` in
  dev-dependencies). vcan0–vcan15 exist on the workstation (`harness/dev-setup.sh`). Taken: vcan0–5 Espar process tests, vcan6–8 the CAN crate. Ask the lead before using one.
- Regenerate `docs/TEST-PARITY.md` with `harness/test-parity.sh` after adding `/// py:` tags.
- Never run anything against the van or a real device. Never `git commit` unless the lead asked you to.
