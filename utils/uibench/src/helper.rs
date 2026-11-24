//! The root half of the benchmark, run by the runner as `sudo -n aetherrv-uibench helper`. It owns the uinput
//! touchscreen and reads process memory (smaps_rollup of sandboxed, non-dumpable browser processes is root-only).
//! Line protocol on stdin/stdout, one reply line per request:
//!
//! ```text
//! touch <w> <h>            → ok <sysname> <event node>
//! down <x> <y> | move <x> <y> | up   → t <ns before write> <ns after write>   (CLOCK_MONOTONIC)
//! sample <cgroup dir>...   → one JSON object (processes, PSS/RSS/USS, cgroup CPU and memory) per cgroup
//! quit                     → exits (EOF too; the touchscreen is destroyed with it)
//! ```
//! It reads only /proc and /sys/fs/cgroup and writes only to its own uinput device.

use crate::uinput::Touch;
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

pub fn main() -> Result<()> {
    let mut touch: Option<Touch> = None;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let p: Vec<&str> = line.split_whitespace().collect();
        let reply = match p.as_slice() {
            ["touch", w, h] => match Touch::create(w.parse()?, h.parse()?) {
                Ok(t) => {
                    let mut node = None;
                    for _ in 0..50 {
                        node = t.event_node();
                        if node.as_deref().is_some_and(|n| std::path::Path::new(n).exists()) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    let r = format!("ok {} {}", t.sysname, node.unwrap_or_default());
                    touch = Some(t);
                    r
                }
                Err(e) => format!("err {e:#}"),
            },
            ["down", x, y] => tap(&mut touch, |t| t.down(x.parse().unwrap_or(0), y.parse().unwrap_or(0))),
            ["move", x, y] => tap(&mut touch, |t| t.moveto(x.parse().unwrap_or(0), y.parse().unwrap_or(0))),
            ["up"] => tap(&mut touch, |t| t.up()),
            ["sample", cgs @ ..] => {
                let v: Vec<Value> = cgs.iter().map(|c| sample_cgroup(c)).collect();
                serde_json::to_string(&v)?
            }
            ["quit"] => break,
            _ => format!("err unknown: {line}"),
        };
        writeln!(out, "{reply}")?;
        out.flush()?;
    }
    drop(touch);
    Ok(())
}

fn tap(t: &mut Option<Touch>, f: impl FnOnce(&mut Touch) -> Result<(u64, u64)>) -> String {
    match t.as_mut() {
        None => "err no touchscreen (send: touch <w> <h>)".into(),
        Some(t) => match f(t) {
            Ok((a, b)) => format!("t {a} {b}"),
            Err(e) => format!("err {e:#}"),
        },
    }
}

fn kv_kb(text: &str, key: &str) -> u64 {
    text.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l[key.len()..].split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn pids_of(cg: &std::path::Path, out: &mut Vec<u32>) {
    if let Ok(s) = std::fs::read_to_string(cg.join("cgroup.procs")) {
        out.extend(s.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
    }
    if let Ok(rd) = std::fs::read_dir(cg) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                pids_of(&e.path(), out);
            }
        }
    }
}

/// PSS / RSS / USS summed over every process in the cgroup (recursively), plus the cgroup's own CPU and memory.
pub fn sample_cgroup(cg: &str) -> Value {
    let path = std::path::Path::new(cg);
    let mut pids = Vec::new();
    pids_of(path, &mut pids);
    let (mut pss, mut rss, mut uss, mut anon, mut file, mut shmem) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut procs = Vec::new();
    for pid in &pids {
        let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) else { continue };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default().trim().to_string();
        let p = kv_kb(&s, "Pss:");
        let u = kv_kb(&s, "Private_Clean:") + kv_kb(&s, "Private_Dirty:");
        pss += p;
        rss += kv_kb(&s, "Rss:");
        uss += u;
        anon += kv_kb(&s, "Pss_Anon:");
        file += kv_kb(&s, "Pss_File:");
        shmem += kv_kb(&s, "Pss_Shmem:");
        // per-process CPU ticks (utime + stime), fields 14 and 15 of /proc/pid/stat
        let ticks = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|st| {
            let rest = &st[st.rfind(')')? + 2..];
            let f: Vec<&str> = rest.split_whitespace().collect();
            Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
        });
        // GPU sanity: a process rendering on the GPU has the render node open; llvmpipe (software GL) maps libLLVM
        let render = std::fs::read_dir(format!("/proc/{pid}/fd")).map(|rd| rd.flatten().any(|e| {
            std::fs::read_link(e.path()).is_ok_and(|l| l.to_string_lossy().starts_with("/dev/dri/renderD"))
        })).unwrap_or(false);
        // libgallium always links libLLVM, so that says nothing; llvmpipe runs its own threads named "llvmpipe-N"
        let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap_or_default();
        let gallium = maps.contains("libgallium");
        let llvmpipe = std::fs::read_dir(format!("/proc/{pid}/task")).map(|rd| rd.flatten().filter(|t| {
            std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.starts_with("llvmpipe"))
        }).count()).unwrap_or(0);
        procs.push(json!({"pid": pid, "comm": comm, "pss_kb": p, "uss_kb": u, "cpu_ticks": ticks,
                          "render_node": render, "gallium": gallium, "llvmpipe_threads": llvmpipe}));
    }
    let cpu_usec = std::fs::read_to_string(path.join("cpu.stat")).ok().and_then(|s| {
        s.lines().find(|l| l.starts_with("usage_usec")).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
    });
    let memcur = std::fs::read_to_string(path.join("memory.current")).ok().and_then(|s| s.trim().parse::<u64>().ok());
    let mempeak = std::fs::read_to_string(path.join("memory.peak")).ok().and_then(|s| s.trim().parse::<u64>().ok());
    // memory.stat: anon / file / shmem (GPU buffers that are shmem-backed show here even when no process maps them)
    let mstat = std::fs::read_to_string(path.join("memory.stat")).unwrap_or_default();
    let stat = |k: &str| mstat.lines().find_map(|l| l.strip_prefix(k).and_then(|v| v.trim().parse::<u64>().ok()));
    // the GPU drivers' own buffer-object totals (system-wide; v3d and vc4 debugfs), for the kept-context comparison
    let bo: Vec<String> = std::fs::read_dir("/sys/kernel/debug/dri").into_iter().flatten().flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("bo_stats")).ok().map(|s| format!("{}: {}", e.file_name().to_string_lossy(), s.split_whitespace().collect::<Vec<_>>().join(" "))))
        .collect();
    json!({
        "cgroup": cg, "t": crate::mono_ns(), "n": procs.len(), "pss_kb": pss, "rss_kb": rss, "uss_kb": uss,
        "pss_anon_kb": anon, "pss_file_kb": file, "pss_shmem_kb": shmem,
        "cpu_usec": cpu_usec, "memory_current": memcur, "memory_peak": mempeak,
        "memory_stat": {"anon": stat("anon "), "file": stat("file "), "shmem": stat("shmem ")}, "gpu_bo_stats": bo, "procs": procs,
    })
}

/// The runner's handle on the root helper.
pub struct Helper {
    child: std::process::Child,
    tx: std::process::ChildStdin,
    rx: std::io::BufReader<std::process::ChildStdout>,
}

impl Helper {
    pub fn spawn() -> Result<Self> {
        let me = std::env::current_exe()?;
        let mut child = std::process::Command::new("sudo")
            .args(["-n", &me.to_string_lossy(), "helper"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let tx = child.stdin.take().unwrap();
        let rx = std::io::BufReader::new(child.stdout.take().unwrap());
        Ok(Helper { child, tx, rx })
    }
    pub fn req(&mut self, line: &str) -> Result<String> {
        writeln!(self.tx, "{line}")?;
        self.tx.flush()?;
        let mut s = String::new();
        self.rx.read_line(&mut s)?;
        if s.is_empty() {
            anyhow::bail!("helper exited");
        }
        Ok(s.trim_end().to_string())
    }
    /// Returns the injection time (ns, CLOCK_MONOTONIC, taken just before the write).
    pub fn touch(&mut self, line: &str) -> Result<u64> {
        let r = self.req(line)?;
        let mut it = r.split_whitespace();
        match it.next() {
            Some("t") => Ok(it.next().and_then(|v| v.parse().ok()).unwrap_or(0)),
            _ => anyhow::bail!("touch {line}: {r}"),
        }
    }
    pub fn sample(&mut self, cgs: &[String]) -> Result<Vec<Value>> {
        let r = self.req(&format!("sample {}", cgs.join(" ")))?;
        Ok(serde_json::from_str(&r)?)
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.tx, "quit");
        let _ = self.tx.flush();
        let _ = self.child.wait();
    }
}
