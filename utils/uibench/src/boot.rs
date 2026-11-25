//! Power-on → usable UI (harness/uibench/boot-measure.sh, boot-time.sh).
//!
//! `boot-report <result.json> [armed]`, on the bench after the reboot: the boot's figures on the kernel's clock
//! (systemd's unit timestamps, the journal, the runner's cold start), written to `<result>-boot.json` and printed;
//! the last line is the JSON boot-time reads.
//!
//! `boot-time <host> <addr> <out.json> <ff>`, on the workstation: reboots the bench, times it from outside (ping, the
//! gateway on :8080, the clock offset over ssh), reads the bench's report and places it on the workstation's clock.

use crate::pyfmt::{Py, fixed};
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn sh(c: &str) -> String {
    Command::new("sh").args(["-c", c]).stdin(Stdio::null()).stderr(Stdio::null()).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn dict(v: Vec<(&str, Py)>) -> Py {
    Py::Dict(v.into_iter().map(|(k, x)| (k.to_string(), x)).collect())
}

/// (launch + offset_ms) / 1000, when both are there
fn since_kernel(launch: &Py, ms: &Py) -> Py {
    launch.num().zip(ms.num()).map_or(Py::None, |(a, b)| Py::Float((a + b) / 1000.0))
}

pub fn report(args: &[String]) -> Result<()> {
    let f = args.first().context("usage: boot-report <result.json> [armed]")?;
    let armed: Vec<&str> = args.get(1).map_or(vec![], |a| a.split_whitespace().collect());
    let d = Py::parse(&std::fs::read_to_string(f).with_context(|| f.clone())?)?;
    let mono_s = |unit: &str, prop: &str| -> Py {
        let v = sh(&format!("systemctl show -p {prop} --value {unit}"));
        match v.parse::<u128>() {
            Ok(n) if n > 0 && v.bytes().all(|b| b.is_ascii_digit()) => Py::Float(n as f64 / 1e6),
            _ => Py::None,
        }
    };
    let first_mono = |unit: Option<&str>, pat: &str| -> Py {
        // no unit: the whole boot's journal (the wait loop runs in the PAM session's scope, not in the unit)
        let u = unit.map_or(String::new(), |u| format!("-u {u}"));
        let v = sh(&format!("journalctl -b -o short-monotonic {u} --no-pager | grep -m1 '{pat}' | tr -d '[]' | awk '{{print $1}}'"));
        v.parse().map_or(Py::None, Py::Float)
    };
    let c = d.at("cold_start");
    let launch = c.at("t_launch_since_kernel_ms");
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")?.split_whitespace().next().unwrap_or("").parse()?;
    let exec = f.replace(".json", ".exec");
    let out = dict(vec![
        ("runtime", d.at("runtime").clone()),
        ("order", armed.get(1).map_or(Py::None, |o| Py::Str(o.to_string()))),
        ("system", Py::Str("rust".into())),
        ("systemd_analyze", Py::Str(sh("systemd-analyze | head -1"))),
        ("kernel_start_epoch", Py::Float(now() - uptime)), // the wall clock when the kernel started
        ("logind_s", mono_s("systemd-logind.service", "ActiveEnterTimestampMonotonic")),
        ("network_target_s", mono_s("network.target", "ActiveEnterTimestampMonotonic")),
        ("user_sessions_s", mono_s("systemd-user-sessions.service", "ActiveEnterTimestampMonotonic")),
        ("default_target_s", mono_s("multi-user.target", "ActiveEnterTimestampMonotonic")),
        ("bus_started_s", mono_s("aetherrv-bus.service", "ActiveEnterTimestampMonotonic")),
        ("backend_started_s", mono_s("aetherrv-gateway.service", "ExecMainStartTimestampMonotonic")),
        ("gateway_serving_s", first_mono(Some("aetherrv-gateway.service"), "web UI on")), // logged when it listens
        ("wait_loop_started_s", first_mono(None, "uibench: wait loop started")),
        ("port_8080_answered_s", first_mono(None, "uibench: :8080 answers")),
        ("loopback_up_s", first_mono(Some("systemd-networkd.service"), " lo: Link UP")),
        ("panel_unit_started_s", mono_s("uibench-boot.service", "ExecMainStartTimestampMonotonic")),
        ("sway_exec_s", std::fs::read_to_string(&exec).ok().and_then(|s| s.trim().parse().ok()).map_or(Py::None, Py::Float)),
        ("runner_setup_ms", d.at("runner_setup_ms").clone()),
        ("runtime_launched_s", launch.num().map(|l| l / 1000.0).into()),
        ("first_data_frame_s", since_kernel(launch, c.at("data_frame_ms"))),
        ("first_usable_frame_s", since_kernel(launch, c.at("settled_ms"))),
        ("cold_start_ms", dict(["first_paint_ms", "data_frame_ms", "content_ms", "settled_ms", "white_flash"].map(|k| (k, c.at(k).clone())).to_vec())),
    ]);
    std::fs::write(f.replace(".json", "-boot.json"), out.dumps(Some(1)))?;
    print!("{}", report_text(f, &out));
    Ok(())
}

fn report_text(f: &str, out: &Py) -> String {
    let mut s = format!("runtime {} on the {} system, ordered as {} ({f}): {}\n", out.at("runtime").str(), out.at("system").str(),
        out.at("order").str(), out.at("systemd_analyze").str());
    for k in ["logind_s", "bus_started_s", "backend_started_s", "gateway_serving_s", "loopback_up_s", "network_target_s", "user_sessions_s",
              "wait_loop_started_s", "port_8080_answered_s", "panel_unit_started_s", "sway_exec_s", "runtime_launched_s", "first_data_frame_s",
              "first_usable_frame_s", "default_target_s"] {
        let v = out.at(k).num().map_or("–".into(), |v| format!("{} s after the kernel started", fixed(v, 2)));
        s += &format!("  {k:24} {v}\n");
    }
    s += &format!("  runner set-up (ms from the runner's start): {}\n", out.at("runner_setup_ms").str());
    s += &dict(vec![("kernel_start_epoch", out.at("kernel_start_epoch").clone()), ("first_usable_frame_s", out.at("first_usable_frame_s").clone())]).dumps(None);
    s + "\n"
}

/// Runs a command (stdin closed) for at most `t` seconds: (exit status, stdout); 255 when it timed out.
fn run_for(c: &mut Command, t: f64) -> (i32, String) {
    let Ok(mut ch) = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return (255, String::new()) };
    let mut so = ch.stdout.take().unwrap();
    let rd = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        String::from_utf8_lossy(&b).to_string()
    });
    let end = Instant::now() + Duration::from_secs_f64(t);
    loop {
        match ch.try_wait() {
            Ok(Some(st)) => return (st.code().unwrap_or(-1), rd.join().unwrap_or_default()),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = ch.kill();
                let _ = ch.wait();
                return (255, String::new());
            }
        }
    }
}

/// What the workstation saw of one reboot; all times are its wall clock.
#[derive(Default)]
struct Seen {
    off0: Option<(f64, f64)>,
    t_cmd: f64,
    last_up: f64,
    first_ping: Option<f64>,
    gw: Option<f64>,
    rep: Option<String>,
    off1: Option<(f64, f64)>,
    journal: String,
    ff_file: String,
}

pub fn time(args: &[String]) -> Result<()> {
    let [host, addr, outf, ff] = args else { anyhow::bail!("usage: boot-time <host> <addr> <out.json> <ff 0|1>") };
    let ff = ff == "1";
    let ssh = |cmd: &str, t: f64| run_for(Command::new("ssh").args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=3", host, cmd]), t);
    // bench clock − workstation clock, from the ssh round trip with the smallest delay
    let offset = || {
        let mut best: Option<(f64, f64)> = None;
        for _ in 0..5 {
            let t1 = now();
            let (rc, out) = ssh("date +%s.%N", 20.0);
            let t2 = now();
            if let (0, Ok(v)) = (rc, out.trim().parse::<f64>())
                && best.is_none_or(|b| t2 - t1 < b.0)
            {
                best = Some((t2 - t1, v - (t1 + t2) / 2.0));
            }
        }
        best
    };
    let ping = || Command::new("ping").args(["-c", "1", "-W", "0.15", addr]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().is_ok_and(|s| s.success());
    let mut s = Seen { off0: offset(), t_cmd: now(), ..Default::default() };
    if ff {
        // the exact wall time, on disk, then reboot(2) at once: no shutdown at all
        ssh("t=$(date +%s.%N); echo $t | sudo tee /var/tmp/uibench-boot/reboot-ff.txt >/dev/null; sync; sudo systemctl reboot -ff", 5.0);
    } else {
        ssh("sudo systemctl reboot", 5.0);
    }
    s.last_up = s.t_cmd;
    while ping() {
        // still answering (the shutdown); ~0.1 s resolution
        s.last_up = now();
        std::thread::sleep(Duration::from_millis(50));
    }
    let down = now();
    while s.first_ping.is_none() && now() - down < 300.0 {
        // the kernel answers ping before any service: ~0.15 s resolution
        let t = now();
        if ping() {
            s.first_ping = Some(t);
        }
    }
    while let Some(fp) = s.first_ping
        && s.gw.is_none()
        && now() - fp < 120.0
    {
        // then the gateway: a closed port is refused at once
        let t = now();
        if gateway(addr) {
            s.gw = Some(t);
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    // the bench's own figures (wait for the boot measurement to have written its result)
    for _ in 0..90 {
        let (rc, out) = ssh("~/uibench/harness/boot-measure.sh report", 20.0);
        if rc == 0 {
            s.rep = Some(out);
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    s.off1 = offset();
    // the firmware on the bench's own clock: the previous boot's last journal entry ("Journal stopped", late in the
    // shutdown) → this kernel's start; an upper bound (the last shutdown steps run after the journal stops)
    s.journal = ssh("journalctl -b -1 -o short-unix -n 1 --no-pager", 20.0).1;
    if ff {
        s.ff_file = ssh("cat /var/tmp/uibench-boot/reboot-ff.txt; sudo rm -f /var/tmp/uibench-boot/reboot-ff.txt", 20.0).1;
    }
    let (res, text) = boot_time_result(&s, ff, outf);
    std::fs::write(outf, res.dumps(Some(1))).with_context(|| outf.clone())?;
    print!("{text}");
    Ok(())
}

/// GET http://addr:8080/ answers 200 within 0.5 s
fn gateway(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let t = Duration::from_millis(500);
    let Some(sa) = (addr, 8080).to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    let Ok(mut c) = std::net::TcpStream::connect_timeout(&sa, t) else { return false };
    let _ = c.set_read_timeout(Some(t));
    let _ = c.set_write_timeout(Some(t));
    if write!(c, "GET / HTTP/1.1\r\nHost: {addr}:8080\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut head = Vec::new();
    let mut b = [0u8; 256];
    while !head.windows(2).any(|w| w == b"\r\n") {
        match c.read(&mut b) {
            Ok(n) if n > 0 => head.extend_from_slice(&b[..n]),
            _ => return false,
        }
    }
    String::from_utf8_lossy(&head).split_whitespace().nth(1) == Some("200")
}

fn first_float(s: &str) -> Option<f64> {
    s.split_whitespace().next()?.parse().ok()
}

/// The result file and the printout of one reboot. Truthiness as written first: a 0 counts as missing.
fn boot_time_result(s: &Seen, ff: bool, outf: &str) -> (Py, String) {
    let nz = |x: Option<f64>| x.filter(|v| *v != 0.0);
    let bench = s.rep.as_deref().filter(|r| !r.is_empty())
        .and_then(|r| Py::parse(r.trim().lines().last().unwrap_or("")).ok()).unwrap_or(Py::Dict(vec![]));
    let journal_stop = first_float(&s.journal);
    let ff_t = if ff { first_float(&s.ff_file) } else { None };
    let o = s.off1.or(s.off0).map_or(0.0, |x| x.1);
    let k = nz(bench.at("kernel_start_epoch").num());
    let k_ws = k.map(|k| k - o);
    let fu = bench.at("first_usable_frame_s");
    let usable_ws = nz(k_ws).zip(nz(fu.num())).map(|(a, b)| a + b);
    let sub = |a: Option<f64>, b: Option<f64>| nz(a).zip(nz(b)).map(|(a, b)| a - b);
    let res = dict(vec![
        ("reboot_command", Py::Float(s.t_cmd)),
        ("last_ping_reply", Py::Float(s.last_up)),
        ("first_ping_reply_after", s.first_ping.into()),
        ("gateway_answering", s.gw.into()),
        ("kernel_start", k_ws.into()),
        ("first_usable_frame", usable_ws.into()),
        ("clock_offset_s", Py::Float(o)),
        ("offset_rtt_s", s.off1.or(s.off0).map(|x| x.0).into()),
        ("firmware_upper_bound_s", nz(k_ws).map(|k| k - s.last_up).into()),
        ("previous_journal_stop_epoch_bench", journal_stop.into()),
        ("firmware_from_journal_s", sub(k, journal_stop).into()),
        ("reboot_ff", Py::Bool(ff)),
        ("reboot_ff_epoch_bench", ff_t.into()),
        ("firmware_from_reboot_ff_s", sub(k, ff_t).into()), // reboot(2) at once → the kernel's start
        ("down_to_gateway_s", nz(s.gw).map(|g| g - s.last_up).into()),
        ("down_to_usable_s", nz(usable_ws).map(|u| u - s.last_up).into()),
        ("kernel_to_usable_s", fu.clone()),
        ("bench_report", s.rep.clone().map_or(Py::None, Py::Str)),
        // the kernel must start between the last reply before the reboot and the first after it, else a clock stepped
        ("kernel_start_plausible", Py::Bool(matches!((nz(k_ws), nz(s.first_ping)), (Some(k), Some(fp)) if s.last_up < k && k < fp))),
        ("polling", Py::Str("ping every ~0.1 s while up, ~0.15 s while down; gateway every ~0.05 s after the first ping".into())),
    ]);
    let f = |x: &Py| x.num().map_or("–".into(), |v| format!("{} s", fixed(v, 2)));
    let rel = |t: Option<f64>| Py::from(t.map(|t| t - s.last_up));
    let mut t = format!("{}\n", s.rep.as_deref().filter(|r| !r.is_empty()).unwrap_or("(no report from the bench)"));
    t += &format!("on the workstation's clock, from the last ping reply before the reboot (clock offset {o:+.3} s, rtt {}):\n", f(res.at("offset_rtt_s")));
    for (name, at) in [("first ping reply", s.first_ping), ("kernel start", k_ws), ("gateway answering", s.gw), ("first usable frame", usable_ws)] {
        t += &format!("  {name:22} {}\n", f(&rel(at)));
    }
    t += &format!("kernel start between the last and first ping replies: {}\n", res.at("kernel_start_plausible").str());
    t += &format!("firmware from the previous boot's last journal entry to the kernel start (bench clock): {}\n", f(res.at("firmware_from_journal_s")));
    if ff {
        t += &format!("firmware from the immediate reboot(2) to the kernel start (bench clock): {}\n", f(res.at("firmware_from_reboot_ff_s")));
    }
    t += &format!("firmware (upper bound: includes the shutdown's end) {}; kernel → usable {}; down → usable {}  ({outf})\n",
        f(res.at("firmware_upper_bound_s")), f(res.at("kernel_to_usable_s")), f(res.at("down_to_usable_s")));
    (res, t)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_reboot() {
        let s = Seen {
            off0: Some((0.08, 0.512)), t_cmd: 1700000000.25, last_up: 1700000003.5, first_ping: Some(1700000016.75), gw: Some(1700000019.0),
            rep: Some("runtime wpe on the rust system, ordered as rust (x): –\n{\"kernel_start_epoch\": 1700000013.0, \"first_usable_frame_s\": 6.25}\n".into()),
            off1: Some((0.05, 0.5)), journal: "1700000010.5 bench systemd-journald[1]: Journal stopped\n".into(), ff_file: String::new(),
        };
        let (res, text) = boot_time_result(&s, false, "/out/boot.json");
        let json = res.dumps(Some(1));
        assert!(json.contains("\n \"kernel_start\": 1700000012.5,\n \"first_usable_frame\": 1700000018.75,\n \"clock_offset_s\": 0.5,\n"), "{json}");
        assert!(json.contains("\n \"firmware_from_journal_s\": 2.5,\n \"reboot_ff\": false,\n \"reboot_ff_epoch_bench\": null,\n"), "{json}");
        assert!(json.contains(r#"(x): \u2013\n{\"kernel_start_epoch\""#), "{json}");
        assert!(text.ends_with("\n\non the workstation's clock, from the last ping reply before the reboot (clock offset +0.500 s, rtt 0.05 s):
  first ping reply       13.25 s
  kernel start           9.00 s
  gateway answering      15.50 s
  first usable frame     15.25 s
kernel start between the last and first ping replies: True
firmware from the previous boot's last journal entry to the kernel start (bench clock): 2.50 s
firmware (upper bound: includes the shutdown's end) 9.00 s; kernel → usable 6.25 s; down → usable 15.25 s  (/out/boot.json)
"), "{text}");
    }

    #[test]
    fn report_lines() {
        let out = dict(vec![("runtime", Py::Str("wpe".into())), ("order", Py::None), ("system", Py::Str("rust".into())),
            ("systemd_analyze", Py::Str("Startup finished".into())), ("kernel_start_epoch", Py::Float(1700000000.5)),
            ("logind_s", Py::Float(3.754)), ("runner_setup_ms", Py::parse(r#"{"launch": 0.0, "launch_first": true}"#).unwrap()),
            ("first_usable_frame_s", Py::Float(7.40261))]);
        let t = report_text("/x/wpe.json", &out);
        assert!(t.starts_with("runtime wpe on the rust system, ordered as None (/x/wpe.json): Startup finished\n  logind_s                 3.75 s after the kernel started\n  bus_started_s            –\n"), "{t}");
        assert!(t.ends_with("  runner set-up (ms from the runner's start): {'launch': 0.0, 'launch_first': True}\n{\"kernel_start_epoch\": 1700000000.5, \"first_usable_frame_s\": 7.40261}\n"), "{t}");
    }
}
