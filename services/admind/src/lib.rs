//! `aetherrv-admind`: the ONLY root-privileged piece of AetherRV. A port of `deploy/admin/aetherrv_admin.py`.
//!
//! Socket-activated (`aetherrv-admind.socket`, `/run/aetherrv-admin.sock`, root:aetherrv 0660). Protocol: one JSON
//! object per connection, `{"verb": str, "args": {...}}` → one JSON reply `{"ok": bool, ...}`. Only uid 0 and the
//! `aetherrv` user may call (SO_PEERCRED). Verbs are a fixed allowlist; every argument is validated; commands are
//! argv lists, never shell strings. Secrets (Wi-Fi passphrases) are never logged, never on a command line, never
//! returned.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Bad(pub String);

impl std::fmt::Display for Bad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(s: impl Into<String>) -> Bad {
    Bad(s.into())
}

pub type R = Result<Map<String, Value>, Bad>;

/// The system the helper acts on (real commands; fakes in tests).
pub trait Sys: Send + Sync {
    /// Run an argv with a timeout → (rc, stdout, stderr). 127 = not installed, 124 = timed out.
    fn run(&self, argv: &[&str], timeout: Duration) -> (i32, String, String);
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
    /// iwd's managed objects (`GetManagedObjects`), as busctl's JSON `data[0]`.
    fn iwd_objects(&self) -> Result<Value, Bad> {
        let (rc, out, _) = self.bus(&["call", IWD, "/", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects"], 10);
        if rc != 0 {
            return Err(bad("Wi-Fi service (iwd) isn't running"));
        }
        serde_json::from_str::<Value>(&out).ok().and_then(|v| v["data"][0].as_object().cloned()).map(Value::Object).ok_or_else(|| bad("Wi-Fi service (iwd) isn't running"))
    }
    fn bus(&self, args: &[&str], timeout: u64) -> (i32, String, String) {
        let t = format!("--timeout={timeout}");
        let mut argv = vec!["busctl", "--json=short", t.as_str()];
        argv.extend_from_slice(args);
        self.run(&argv, Duration::from_secs(timeout + 5))
    }
}

pub struct RealSys;

impl Sys for RealSys {
    fn run(&self, argv: &[&str], timeout: Duration) -> (i32, String, String) {
        use std::process::{Command, Stdio};
        let mut c = match Command::new(argv[0]).args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (127, String::new(), format!("{} is not installed", argv[0])),
            Err(e) => return (1, String::new(), e.to_string()),
        };
        let end = std::time::Instant::now() + timeout;
        loop {
            match c.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() >= end => {
                    let _ = c.kill();
                    let _ = c.wait();
                    return (124, String::new(), format!("{} timed out", argv[0]));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return (1, String::new(), e.to_string()),
            }
        }
        let out = c.wait_with_output().unwrap();
        (out.status.code().unwrap_or(1), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
    }
}

pub const IWD: &str = "net.connman.iwd";

pub struct Helper {
    pub sys: Box<dyn Sys>,
    pub zoneinfo: PathBuf,
    pub iwd_dir: PathBuf,
    pub update_run: PathBuf,
    pub update_tmp: PathBuf,
    pub pacman_sync: PathBuf,
    pub allowed_users: Vec<String>,
    pub kiosk_unit: String,
}

impl Helper {
    pub fn new(sys: Box<dyn Sys>) -> Self {
        Helper {
            sys,
            zoneinfo: "/usr/share/zoneinfo".into(),
            iwd_dir: "/var/lib/iwd".into(),
            update_run: "/run/aetherrv-update".into(),
            update_tmp: "/var/tmp/aetherrv-checkupdates".into(),
            pacman_sync: "/var/lib/pacman/sync".into(),
            allowed_users: vec!["aetherrv".into()],
            kiosk_unit: std::env::var("AETHERRV_KIOSK_UNIT").unwrap_or_else(|_| "aetherrv-panel.service".into()),
        }
    }

    fn run(&self, argv: &[&str]) -> (i32, String, String) {
        self.sys.run(argv, Duration::from_secs(30))
    }

    fn run_t(&self, argv: &[&str], t: u64) -> (i32, String, String) {
        self.sys.run(argv, Duration::from_secs(t))
    }

    pub fn verb(&self, verb: &str, a: &Map<String, Value>) -> Option<R> {
        Some(match verb {
            "can_reset" => self.can_reset(a),
            "can_rebind" => self.can_rebind(a),
            "status" => self.status(),
            "set_hostname" => self.set_hostname(a),
            "set_timezone" => self.set_timezone(a),
            "reboot" => self.later(&["systemctl", "reboot"], "rebooting"),
            "poweroff" => self.later(&["systemctl", "poweroff"], "powering_off"),
            "restart_kiosk" => {
                let u = self.kiosk_unit.clone();
                self.later(&["systemctl", "restart", &u], "restarting")
            }
            "wifi_status" => self.wifi_status(),
            "wifi_scan" => self.wifi_scan(),
            "wifi_power" => self.wifi_power(a),
            "wifi_connect" => self.wifi_connect(a),
            "wifi_forget" => self.wifi_forget(a),
            "tailscale_status" => self.tailscale_status(),
            "tailscale_install" => self.tailscale_install(),
            "tailscale_login" => self.tailscale_login(),
            "tailscale_logout" => self.tailscale_logout(),
            "tailscale_whois" => self.tailscale_whois(a),
            "update_check" => self.update_check(),
            "update_start" => self.update_start(),
            "update_status" => self.update_status(),
            _ => return None,
        })
    }

    // ── system ──────────────────────────────────────────────────────────────────────────────────────────────────
    fn status(&self) -> R {
        let (_, host, _) = self.run(&["hostnamectl", "hostname"]);
        let (_, tz, _) = self.run(&["timedatectl", "show", "-p", "Timezone", "--value"]);
        let (_, ntp, _) = self.run(&["timedatectl", "show", "-p", "NTPSynchronized", "--value"]);
        let (_, addrs, _) = self.run(&["ip", "-j", "addr"]);
        let up = std::fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
        let mut pk = Map::new();
        for name in ["iwd", "tailscale", "sway", "chromium", "python", "pacman-contrib", "linux-rpi"] {
            let (rc, out, _) = self.run(&["pacman", "-Q", name]);
            let v = out.split_whitespace().nth(1).filter(|_| rc == 0).map(|s| json!(s)).unwrap_or(Value::Null);
            pk.insert(name.into(), v);
        }
        let mut ifs = vec![];
        if let Ok(Value::Array(list)) = serde_json::from_str::<Value>(if addrs.is_empty() { "[]" } else { &addrs }) {
            for i in list {
                let n = i["ifname"].as_str().unwrap_or("");
                if n == "lo" || n.starts_with("vcan") || n.starts_with("hat") {
                    continue;
                }
                let v4: Vec<Value> = i["addr_info"].as_array().map(|a| a.iter().filter(|x| x["family"] == "inet").map(|x| x["local"].clone()).collect()).unwrap_or_default();
                ifs.push(json!({"name": i["ifname"], "state": i["operstate"], "ipv4": v4}));
            }
        }
        let (total, free) = statvfs("/");
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
        Ok(json!({"hostname": host.trim(), "timezone": tz.trim(), "ntp_synced": ntp.trim() == "yes", "kernel": kernel,
                  "uptime_s": up, "interfaces": ifs, "packages": pk, "disk": {"total": total, "free": free}})
        .as_object()
        .unwrap()
        .clone())
    }

    fn set_hostname(&self, a: &Map<String, Value>) -> R {
        let name = valid_str(a, "name", 64, is_hostname)?;
        let (rc, _, err) = self.run(&["hostnamectl", "set-hostname", &name]);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "hostnamectl failed")));
        }
        Ok(obj(json!({"hostname": name})))
    }

    fn set_timezone(&self, a: &Map<String, Value>) -> R {
        let tz = valid_str(a, "tz", 64, is_tz)?;
        let path = std::fs::canonicalize(self.zoneinfo.join(&tz)).map_err(|_| bad("unknown time zone"))?;
        let zi = std::fs::canonicalize(&self.zoneinfo).unwrap_or(self.zoneinfo.clone());
        if !path.starts_with(&zi) || path == zi || !path.is_file() {
            return Err(bad("unknown time zone"));
        }
        let (rc, _, err) = self.run(&["timedatectl", "set-timezone", &tz]);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "timedatectl failed")));
        }
        Ok(obj(json!({"timezone": tz})))
    }

    /// Reply first, then act (the caller wants its answer before the system goes away).
    fn later(&self, argv: &[&str], key: &str) -> R {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            let a: Vec<&str> = argv.iter().map(String::as_str).collect();
            RealSys.run(&a, Duration::from_secs(60));
        });
        Ok(obj(json!({key: true})))
    }

    // ── Wi-Fi (iwd over D-Bus via busctl; networkd does addresses) ─────────────────────────────────────────────
    fn iwd_device(objs: &Value) -> Option<(String, Value)> {
        let mut v: Vec<(&String, &Value)> = objs.as_object()?.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v.into_iter().find(|(_, i)| i.get("net.connman.iwd.Device").is_some()).map(|(p, i)| (p.clone(), i.clone()))
    }

    fn wifi_status(&self) -> R {
        let objs = self.sys.iwd_objects()?;
        let Some((path, ifs)) = Self::iwd_device(&objs) else { return Ok(obj(json!({"present": false}))) };
        let d = &ifs["net.connman.iwd.Device"];
        let st = ifs.get("net.connman.iwd.Station").cloned().unwrap_or(json!({}));
        let known: Vec<Value> = objs
            .as_object()
            .unwrap()
            .values()
            .filter_map(|i| i.get("net.connman.iwd.KnownNetwork"))
            .map(|k| json!({"ssid": p(k, "Name"), "security": p(k, "Type")}))
            .collect();
        let mut nets = vec![];
        if st.as_object().is_some_and(|m| !m.is_empty()) {
            let (rc, out, _) = self.sys.bus(&["call", IWD, &path, "net.connman.iwd.Station", "GetOrderedNetworks"], 10);
            let list = if rc == 0 { serde_json::from_str::<Value>(&out).ok().map(|v| v["data"][0].clone()) } else { None };
            for e in list.and_then(|l| l.as_array().cloned()).unwrap_or_default() {
                let (np, sig) = (e[0].as_str().unwrap_or(""), e[1].as_f64().unwrap_or(0.0));
                if let Some(n) = objs.get(np).and_then(|o| o.get("net.connman.iwd.Network")) {
                    nets.push(json!({"ssid": p(n, "Name"), "security": p(n, "Type"), "signal_dbm": (sig / 100.0).round() as i64,
                                     "known": truthy(&p(n, "KnownNetwork")), "connected": truthy(&p(n, "Connected"))}));
                }
            }
        }
        let conn = nets.iter().find(|n| n["connected"] == true).map(|n| n["ssid"].clone()).unwrap_or(Value::Null);
        Ok(obj(json!({"present": true, "device": p(d, "Name"), "powered": truthy(&p(d, "Powered")), "state": p(&st, "State"),
                      "scanning": truthy(&p(&st, "Scanning")), "connected": conn, "networks": nets, "known": known})))
    }

    fn wifi_scan(&self) -> R {
        let Some((path, _)) = Self::iwd_device(&self.sys.iwd_objects()?) else { return Err(bad("no Wi-Fi device")) };
        self.sys.bus(&["call", IWD, &path, "net.connman.iwd.Station", "Scan"], 10); // Busy is fine: a scan is running
        Ok(obj(json!({"scanning": true})))
    }

    fn wifi_power(&self, a: &Map<String, Value>) -> R {
        let Some(on) = a.get("on").and_then(Value::as_bool) else { return Err(bad("invalid on")) };
        let Some((path, _)) = Self::iwd_device(&self.sys.iwd_objects()?) else { return Err(bad("no Wi-Fi device")) };
        let (rc, _, err) = self.sys.bus(&["set-property", IWD, &path, "net.connman.iwd.Device", "Powered", "b", if on { "true" } else { "false" }], 30);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "couldn't change Wi-Fi power")));
        }
        Ok(obj(json!({"powered": on})))
    }

    pub fn iwd_file(&self, ssid: &str, kind: &str) -> PathBuf {
        // iwd(5) network file naming: plain if only [A-Za-z0-9 _-], else "=" + hex of the SSID bytes
        let plain = !ssid.is_empty() && ssid.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '_' || c == '-');
        let name = if plain { ssid.to_string() } else { format!("={}", hex(ssid.as_bytes())) };
        self.iwd_dir.join(format!("{name}.{kind}"))
    }

    fn wifi_connect(&self, a: &Map<String, Value>) -> R {
        let ssid = ssid(a)?;
        let objs = self.sys.iwd_objects()?;
        let net = objs.as_object().unwrap().iter().find_map(|(pth, i)| {
            let n = i.get("net.connman.iwd.Network")?;
            (p(n, "Name") == json!(ssid)).then(|| (pth.clone(), n.clone()))
        });
        let Some((npath, n)) = net else { return Err(bad("that network isn't in range — scan again")) };
        let typ = p(&n, "Type");
        if typ == "8021x" {
            return Err(bad("enterprise (802.1X) Wi-Fi isn't supported"));
        }
        let mut created: Option<PathBuf> = None;
        if typ == "psk" && !truthy(&p(&n, "KnownNetwork")) {
            let pw = a.get("passphrase").and_then(Value::as_str);
            let (psk, phrase) = match pw {
                Some(pw) if pw.len() == 64 && pw.chars().all(|c| c.is_ascii_hexdigit()) => (pw.to_lowercase(), None), // raw PSK: WPA2 only
                Some(pw) if (8..=63).contains(&pw.chars().count()) && pw.chars().all(|c| (32..127).contains(&(c as u32))) => {
                    (wpa_psk(pw, &ssid), Some(pw.to_string()))
                }
                _ => return Err(bad("the password must be 8–63 characters")),
            };
            let fpath = self.iwd_file(&ssid, "psk");
            // Passphrase for WPA3/SAE (and transition-mode APs), PreSharedKey for WPA2. iwd's settings format escapes
            // backslash as \\ and space as \s (so leading/trailing spaces survive).
            let body = match phrase {
                Some(ph) => format!("[Security]\nPassphrase={}\nPreSharedKey={psk}\n", ph.replace('\\', "\\\\").replace(' ', "\\s")),
                None => format!("[Security]\nPreSharedKey={psk}\n"),
            };
            write_0600(&fpath, &body).map_err(|e| bad(format!("couldn't save the network: {e}")))?;
            created = Some(fpath);
            for _ in 0..30 {
                // iwd picks the file up via inotify
                let o = self.sys.iwd_objects().unwrap_or(json!({}));
                if truthy(&p(&o.get(&npath).and_then(|x| x.get("net.connman.iwd.Network")).cloned().unwrap_or(json!({})), "KnownNetwork")) {
                    break;
                }
                self.sys.sleep(Duration::from_millis(100));
            }
        }
        let (rc, _, err) = self.sys.bus(&["call", IWD, &npath, "net.connman.iwd.Network", "Connect"], 40);
        if rc != 0 {
            if let Some(c) = created {
                let _ = std::fs::remove_file(c); // a wrong password must not stay saved
            }
            let e = err.trim();
            return Err(bad(if e.contains("Failed") || e.contains("InvalidFormat") || e.contains("Aborted") {
                "couldn't join — check the password".to_string()
            } else {
                non_empty(e, "couldn't join")
            }));
        }
        Ok(obj(json!({"connected": ssid})))
    }

    fn wifi_forget(&self, a: &Map<String, Value>) -> R {
        let ssid = ssid(a)?;
        let objs = self.sys.iwd_objects()?;
        let k = objs.as_object().unwrap().iter().find_map(|(pth, i)| {
            let k = i.get("net.connman.iwd.KnownNetwork")?;
            (p(k, "Name") == json!(ssid)).then(|| pth.clone())
        });
        let Some(k) = k else { return Err(bad("not a saved network")) };
        let (rc, _, err) = self.sys.bus(&["call", IWD, &k, "net.connman.iwd.KnownNetwork", "Forget"], 10);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "couldn't forget")));
        }
        Ok(obj(json!({"forgot": ssid})))
    }

    // ── Tailscale ───────────────────────────────────────────────────────────────────────────────────────────────
    fn ts_status(&self) -> Option<Value> {
        let (_, out, _) = self.run_t(&["tailscale", "status", "--json"], 10);
        if out.trim().is_empty() { None } else { serde_json::from_str(&out).ok() }
    }

    fn tailscale_status(&self) -> R {
        let (rc, out, _) = self.run(&["pacman", "-Q", "tailscale"]);
        if rc != 0 {
            return Ok(obj(json!({"installed": false})));
        }
        let (_, act, _) = self.run(&["systemctl", "is-active", "tailscaled"]);
        let st = self.ts_status().unwrap_or(json!({}));
        let me = st.get("Self").cloned().unwrap_or(json!({}));
        let uid = me.get("UserID").map(|v| v.to_string()).unwrap_or_default();
        let user = st.get("User").and_then(|u| u.get(&uid)).and_then(|u| u.get("LoginName")).cloned().unwrap_or(Value::Null);
        let dns = me.get("DNSName").and_then(Value::as_str).map(|s| s.trim_end_matches('.')).filter(|s| !s.is_empty());
        Ok(obj(json!({"installed": true, "version": out.split_whitespace().nth(1), "running": act.trim() == "active",
                      "state": st.get("BackendState"), "auth_url": st.get("AuthURL").filter(|v| truthy(v)),
                      "name": me.get("HostName"), "dns": dns, "ips": me.get("TailscaleIPs").cloned().unwrap_or(json!([])),
                      "online": truthy(&me.get("Online").cloned().unwrap_or(Value::Null)),
                      "tailnet": st.get("CurrentTailnet").and_then(|t| t.get("Name")), "user": user})))
    }

    fn tailscale_install(&self) -> R {
        // from the already-synced package lists only (never a bare -Sy: no partial upgrades)
        let (rc, out, err) = self.run_t(&["pacman", "-S", "--needed", "--noconfirm", "tailscale"], 600);
        if rc != 0 {
            let t = if err.trim().is_empty() { out } else { err };
            return Err(bad(t.trim().lines().last().unwrap_or("install failed").to_string()));
        }
        let (rc, _, err) = self.run_t(&["systemctl", "enable", "--now", "tailscaled.service"], 60);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "couldn't start tailscaled")));
        }
        Ok(obj(json!({"installed": true})))
    }

    /// Start `tailscale up` in the background (it waits for the browser sign-in) and return the login URL. No subnet
    /// routes / exit node are accepted or advertised.
    fn tailscale_login(&self) -> R {
        let st = self.ts_status().unwrap_or(json!({}));
        if st.get("BackendState") == Some(&json!("Running")) {
            return Ok(obj(json!({"state": "Running"})));
        }
        let (_, host, _) = self.run(&["hostnamectl", "hostname"]);
        let mut name: String = host.trim().to_lowercase().chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' { c } else { '-' }).collect();
        if name.is_empty() {
            name = "aetherrv".into();
        }
        use std::os::unix::process::CommandExt;
        let _ = std::process::Command::new("tailscale")
            .args(["up", &format!("--hostname={name}"), "--accept-routes=false", "--accept-dns=false"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn();
        let mut st = json!({});
        for _ in 0..40 {
            self.sys.sleep(Duration::from_millis(500));
            st = self.ts_status().unwrap_or(json!({}));
            if st.get("AuthURL").is_some_and(truthy) || st.get("BackendState") == Some(&json!("Running")) {
                break;
            }
        }
        Ok(obj(json!({"state": st.get("BackendState"), "auth_url": st.get("AuthURL").filter(|v| truthy(v))})))
    }

    /// Who is behind a Tailscale address, and is it the account this console is signed in with? owner = same user
    /// ID as this node AND not a tagged device.
    fn tailscale_whois(&self, a: &Map<String, Value>) -> R {
        let ip: std::net::IpAddr = a.get("ip").and_then(Value::as_str).unwrap_or("").parse().map_err(|_| bad("invalid ip"))?;
        let ts = match ip {
            std::net::IpAddr::V4(v) => (u32::from(v) & 0xFFC0_0000) == 0x6440_0000,
            std::net::IpAddr::V6(v) => (u128::from(v) >> 80) == 0xfd7a_115c_a1e0,
        };
        if !ts {
            return Err(bad("not a Tailscale address"));
        }
        let ips = ip.to_string();
        let (rc, out, _) = self.run_t(&["tailscale", "whois", "--json", &ips], 10);
        if rc != 0 {
            return Ok(obj(json!({"known": false})));
        }
        let Ok(w) = serde_json::from_str::<Value>(&out) else { return Ok(obj(json!({"known": false}))) };
        let st = self.ts_status().unwrap_or(json!({}));
        let me = st.get("Self").and_then(|s| s.get("UserID")).cloned();
        let (node, prof) = (w.get("Node").cloned().unwrap_or(json!({})), w.get("UserProfile").cloned().unwrap_or(json!({})));
        let tagged = node.get("Tags").is_some_and(truthy);
        let owner = !tagged && me.is_some() && me.as_ref() != Some(&Value::Null) && prof.get("ID") == me.as_ref();
        let nm = node.get("Name").and_then(Value::as_str).unwrap_or("").split('.').next().unwrap_or("").to_string();
        Ok(obj(json!({"known": true, "owner": owner, "login": prof.get("LoginName"), "node": nm, "tagged": tagged,
                      "self_ips": st.get("Self").and_then(|s| s.get("TailscaleIPs")).cloned().unwrap_or(json!([]))})))
    }

    fn tailscale_logout(&self) -> R {
        let (rc, _, err) = self.run(&["tailscale", "logout"]);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "logout failed")));
        }
        Ok(obj(json!({"logged_out": true})))
    }

    // ── System updates (pacman; kernel/boot packages held in IgnorePkg) ────────────────────────────────────────
    /// Like checkupdates(8): sync a throwaway copy of the package lists, never the system's (no partial upgrades).
    fn update_check(&self) -> R {
        let tmp = &self.update_tmp;
        let _ = std::fs::create_dir_all(tmp.join("sync"));
        let local = tmp.join("local");
        if std::fs::symlink_metadata(&local).map(|m| !m.file_type().is_symlink()).unwrap_or(true) {
            let _ = std::os::unix::fs::symlink("/var/lib/pacman/local", &local);
        }
        if let Ok(rd) = std::fs::read_dir(&self.pacman_sync) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().ends_with(".db") {
                    let _ = std::fs::copy(e.path(), tmp.join("sync").join(e.file_name()));
                }
            }
        }
        let t = tmp.to_string_lossy().to_string();
        let (rc, out, err) = self.run_t(&["pacman", "-Sy", "--dbpath", &t, "--logfile", "/dev/null", "--noconfirm"], 300);
        if rc != 0 {
            let both = format!("{err}{out}");
            return Err(bad(if both.contains("failed retrieving") { "couldn't reach the package servers".to_string() } else { non_empty(err.trim(), "check failed") }));
        }
        let (_, out, _) = self.run_t(&["pacman", "-Qu", "--dbpath", &t], 60);
        let (mut ups, mut held) = (vec![], vec![]);
        for line in out.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() >= 4 && f[2] == "->" && !f[0].is_empty() && !f[1].is_empty() && !f[3].is_empty() {
                let e = json!({"name": f[0], "old": f[1], "new": f[3]});
                if f.get(4) == Some(&"[ignored]") { held.push(e) } else { ups.push(e) }
            }
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let res = json!({"updates": ups, "held": held, "checked": now});
        let _ = std::fs::create_dir_all(&self.update_run);
        let _ = std::fs::write(self.update_run.join("check.json"), res.to_string());
        Ok(obj(res))
    }

    fn update_start(&self) -> R {
        if read_kv(&self.update_run.join("state")).get("status").and_then(Value::as_str) == Some("running") {
            return Err(bad("an update is already running"));
        }
        let (rc, _, err) = self.run(&["systemctl", "start", "--no-block", "aetherrv-update.service"]);
        if rc != 0 {
            return Err(bad(non_empty(err.trim(), "couldn't start the update")));
        }
        Ok(obj(json!({"started": true})))
    }

    fn update_status(&self) -> R {
        let mut st = read_kv(&self.update_run.join("state"));
        let (_, act, _) = self.run(&["systemctl", "is-active", "aetherrv-update.service"]);
        if st.get("status").and_then(Value::as_str) == Some("running") && !matches!(act.trim(), "active" | "activating") {
            st.insert("status".into(), json!("failed")); // the unit died without finishing
        }
        let log: Vec<String> = std::fs::read_to_string(self.update_run.join("log")).map(|s| {
            let l: Vec<String> = s.lines().map(str::to_string).collect();
            l[l.len().saturating_sub(40)..].to_vec()
        }).unwrap_or_default();
        let check = std::fs::read_to_string(self.update_run.join("check.json")).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let last = read_kv(Path::new("/var/lib/aetherrv-update/last"));
        Ok(obj(json!({"state": st, "log": log, "check": check, "last": last})))
    }

    // ── CAN ports ───────────────────────────────────────────────────────────────────────────────────────────────
    fn can_iface(a: &Map<String, Value>) -> Result<String, Bad> {
        match a.get("iface").and_then(Value::as_str) {
            Some(i @ ("hat0" | "hat1" | "fd0" | "fd1")) => Ok(i.to_string()),
            _ => Err(bad("invalid iface")),
        }
    }

    /// Down/up a local CAN port: discards a frame stuck waiting for an ACK (the MCP2515 retries forever).
    fn can_reset(&self, a: &Map<String, Value>) -> R {
        let iface = Self::can_iface(a)?;
        for st in ["down", "up"] {
            let (rc, _, err) = self.run_t(&["ip", "link", "set", &iface, st], 10);
            if rc != 0 {
                return Err(bad(non_empty(err.trim(), &format!("ip link set {st} failed"))));
            }
        }
        Ok(obj(json!({"iface": iface})))
    }

    /// Unbind + rebind a CAN port's SPI driver: a full chip re-init.
    fn can_rebind(&self, a: &Map<String, Value>) -> R {
        let iface = Self::can_iface(a)?;
        let dev = std::fs::canonicalize(format!("/sys/class/net/{iface}/device")).map_err(|_| bad("no SPI device for that interface"))?;
        let drv = std::fs::canonicalize(dev.join("driver")).map_err(|_| bad("no SPI device for that interface"))?;
        let name = dev.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !name.starts_with("spi") || !drv.is_dir() {
            return Err(bad("no SPI device for that interface"));
        }
        std::fs::write(drv.join("unbind"), &name).map_err(|e| bad(e.to_string()))?;
        self.sys.sleep(Duration::from_millis(500));
        std::fs::write(drv.join("bind"), &name).map_err(|e| bad(e.to_string()))?;
        self.sys.sleep(Duration::from_secs(2));
        self.run_t(&["networkctl", "reconfigure", &iface], 15);
        Ok(obj(json!({"iface": iface, "device": name})))
    }
}

/// Read-only verbs: not logged.
pub const QUIET: [&str; 6] = ["status", "wifi_status", "wifi_scan", "tailscale_status", "update_status", "tailscale_whois"];

/// One request → one reply (`who` = the allowed caller's name, or None).
pub fn handle_line(h: &Helper, who: Option<&str>, line: &[u8]) -> Value {
    let Some(who) = who else { return json!({"ok": false, "error": "not allowed"}) };
    let req: Value = match serde_json::from_slice(if line.iter().all(u8::is_ascii_whitespace) { b"{}" } else { line }) {
        Ok(v) => v,
        Err(e) => return json!({"ok": false, "error": format!("bad request: {e}")}),
    };
    let verb = req.get("verb").and_then(Value::as_str);
    let args = match req.get("args") {
        None | Some(Value::Null) => Some(Map::new()),
        Some(Value::Object(m)) if m.is_empty() => Some(Map::new()),
        Some(Value::Object(m)) => Some(m.clone()),
        _ => None,
    };
    let (Some(verb), Some(args)) = (verb, args) else { return json!({"ok": false, "error": "unknown verb"}) };
    let Some(r) = h.verb(verb, &args) else { return json!({"ok": false, "error": "unknown verb"}) };
    let reply = match r {
        Ok(mut m) => {
            m.insert("ok".into(), json!(true));
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.extend(m.into_iter().filter(|(k, _)| k != "ok"));
            Value::Object(out)
        }
        Err(e) => json!({"ok": false, "error": e.0}),
    };
    if !QUIET.contains(&verb) {
        let what = if reply["ok"] == true { "ok".to_string() } else { format!("refused: {}", reply["error"].as_str().unwrap_or("")) };
        println!("{who}: {verb} {what}");
    }
    reply
}

/// The caller's user name if allowed (root, or one of `allowed`), from SO_PEERCRED.
pub fn peer_ok(fd: std::os::fd::RawFd, allowed: &[String]) -> Option<String> {
    let mut cred = libc::ucred { pid: 0, uid: u32::MAX, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes a ucred into our buffer.
    let rc = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut _, &mut len) };
    if rc != 0 {
        return None;
    }
    if cred.uid == 0 {
        return Some("root".into());
    }
    let name = user_name(cred.uid)?;
    allowed.contains(&name).then_some(name)
}

pub fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0i8; 4096];
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwuid_r fills pw/buf we own.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut res) };
    if rc != 0 || res.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(pw.pw_name) }.to_string_lossy().to_string())
}

/// Serve one connection (blocking, on its own thread).
pub fn serve_conn(h: &Helper, mut s: std::os::unix::net::UnixStream) {
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    let who = peer_ok(s.as_raw_fd(), &h.allowed_users);
    let mut buf = vec![];
    let mut chunk = [0u8; 4096];
    while !buf.ends_with(b"\n") && buf.len() < 65536 {
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let reply = handle_line(h, who.as_deref(), &buf);
    let mut out = reply.to_string().into_bytes();
    out.push(b'\n');
    let _ = s.write_all(&out);
}

// ── helpers ─────────────────────────────────────────────────────────────────────────────────────────────────────

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn non_empty(s: &str, d: &str) -> String {
    if s.is_empty() { d.to_string() } else { s.to_string() }
}

fn p(props: &Value, key: &str) -> Value {
    props.get(key).and_then(|v| v.get("data")).cloned().unwrap_or(Value::Null)
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

fn valid_str(a: &Map<String, Value>, key: &str, maxlen: usize, ok: fn(&str) -> bool) -> Result<String, Bad> {
    match a.get(key).and_then(Value::as_str) {
        Some(v) if !v.is_empty() && v.chars().count() <= maxlen && ok(v) => Ok(v.to_string()),
        _ => Err(bad(format!("invalid {key}"))),
    }
}

/// `[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?`
pub fn is_hostname(s: &str) -> bool {
    let b = s.as_bytes();
    let alnum = |c: u8| c.is_ascii_alphanumeric();
    !b.is_empty() && b.len() <= 63 && alnum(b[0]) && alnum(b[b.len() - 1]) && b.iter().all(|c| alnum(*c) || *c == b'-')
}

/// `[A-Za-z_]+(/[A-Za-z0-9_+-]+){0,2}`
pub fn is_tz(s: &str) -> bool {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    parts[0].chars().all(|c| c.is_ascii_alphabetic() || c == '_')
        && parts[1..].iter().all(|p| p.chars().all(|c| c.is_ascii_alphanumeric() || "_+-".contains(c)))
}

fn ssid(a: &Map<String, Value>) -> Result<String, Bad> {
    match a.get("ssid").and_then(Value::as_str) {
        Some(v) if !v.is_empty() && v.len() <= 32 && !v.chars().any(|c| (c as u32) < 32) => Ok(v.to_string()),
        _ => Err(bad("invalid network name")),
    }
}

/// WPA PSK = PBKDF2-HMAC-SHA1(passphrase, SSID, 4096, 32).
pub fn wpa_psk(pass: &str, ssid: &str) -> String {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(pass.as_bytes(), ssid.as_bytes(), 4096, &mut out);
    hex(&out)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn write_0600(p: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(p)?;
    f.write_all(body.as_bytes())
}

fn read_kv(p: &Path) -> Map<String, Value> {
    std::fs::read_to_string(p)
        .map(|s| s.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), json!(v))).collect())
        .unwrap_or_default()
}

fn statvfs(p: &str) -> (u64, u64) {
    let c = std::ffi::CString::new(p).unwrap();
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: statvfs fills our struct.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return (0, 0);
    }
    (s.f_blocks * s.f_frsize, s.f_bavail * s.f_frsize)
}
