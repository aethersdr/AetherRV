//! `aetherrv-setup`: one-time setup steps on the Pi, so a Rust-only console needs no Python. Run with sudo: each
//! writes /etc/aetherrv/secrets.toml (root:aetherrv 0640). No secret is ever printed or put on a command line.
//!
//! ```text
//! aetherrv-setup victron-pair <gx-address> [role]
//!     pair with a Victron GX (Venus OS >= 3.80 "MQTT pairing"); stores victron_mqtt_user / victron_mqtt_password
//! aetherrv-setup peplink-rw-client <router-address>
//!     create the Peplink read-write API client AetherRV uses ONLY for WAN priority changes; asks for the router's
//!     admin login (used for this one session, never stored); stores peplink_rw_client_id / peplink_rw_client_secret
//! aetherrv-setup store-secrets
//!     `key=value` lines on stdin replace those keys (the tools/ scripts send theirs this way over ssh)
//! ```
//!
//! victron-pair: then, on the GX: Settings → Integrations → MQTT → enable pairing mode (open for 120 s). This retries
//! `POST https://<gx>/auth/generate-token/` (form: role, device_id) every 5 s for up to 4 minutes, the request the
//! victron-mqtt library makes. The GX's and the router's certificates are self-signed, so they are not verified.
//!
//! Every write keeps the file's other lines and is checked with the parser the services use before it replaces the
//! file: one they can't parse would leave every service without its secrets. The previous file is kept as
//! secrets.toml.bak (root 0600).

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SECRETS: &str = "/etc/aetherrv/secrets.toml";
/// The read-write API client's name, as the router lists it.
const RW_CLIENT_NAME: &str = "AetherRV WAN priority";

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "pi".into())
}

fn need_root() -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("Run with sudo (it writes {SECRETS}).");
    }
    Ok(())
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The secrets text with `pairs` replacing any earlier lines for those keys; every other line kept as it was.
fn merged(old: &str, pairs: &[(String, String)]) -> String {
    let key = |l: &str| l.split('=').next().unwrap_or("").trim().to_string();
    let mut lines: Vec<String> = old.lines().filter(|l| !pairs.iter().any(|(k, _)| key(l) == *k)).map(str::to_string).collect();
    for (k, v) in pairs {
        lines.push(format!("{k} = {}", toml_str(v)));
    }
    lines.join("\n") + "\n"
}

/// A TOML basic string (control characters, DEL included, escaped: TOML allows none of them raw).
fn toml_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o += "\\\"",
            '\\' => o += "\\\\",
            '\n' => o += "\\n",
            '\t' => o += "\\t",
            '\r' => o += "\\r",
            c if (c as u32) < 0x20 || c == '\u{7f}' => o += &format!("\\u{:04X}", c as u32),
            c => o.push(c),
        }
    }
    o + "\""
}

/// The services' own parser must read the new text, and every pair back exactly (a key appended after a `[table]`
/// would land inside it). The parser's error is not shown: it quotes the offending line, which may hold a secret.
fn check(text: &str, pairs: &[(String, String)]) -> anyhow::Result<()> {
    let Ok(c) = aetherrv_core::config::Cfg::parse(text) else {
        anyhow::bail!("the secrets file would not parse as TOML (is an existing line malformed?); nothing written");
    };
    for (k, v) in pairs {
        if c.opt_str(k).as_deref() != Some(v.as_str()) {
            anyhow::bail!("{k} would not read back as written (is it after a [table]?); nothing written");
        }
    }
    Ok(())
}

/// A fresh root-owned file (removed first, so the mode applies), synced.
fn write_file(path: &str, text: &str, mode: u32, gid: u32) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    let c = std::ffi::CString::new(path)?;
    if unsafe { libc::chown(c.as_ptr(), 0, gid) } != 0 {
        anyhow::bail!("chown {path}: {}", std::io::Error::last_os_error());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn store(path: &str, pairs: &[(String, String)]) -> anyhow::Result<()> {
    if let Some((k, _)) = pairs.iter().find(|(k, _)| !valid_key(k)) {
        anyhow::bail!("not a plain key: {k:?}");
    }
    let old = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => anyhow::bail!("cannot read {path}: {e}"),
    };
    let new = merged(&old, pairs);
    check(&new, pairs)?;
    let gid = group_id("aetherrv").ok_or_else(|| anyhow::anyhow!("no group aetherrv"))?;
    if !old.is_empty() {
        write_file(&format!("{path}.bak"), &old, 0o600, 0)?;
    }
    let tmp = format!("{path}.new");
    write_file(&tmp, &new, 0o640, gid)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `key=value` lines (the value is everything after the first `=`); blank lines are skipped.
fn read_pairs(input: impl BufRead) -> anyhow::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for l in input.lines() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { anyhow::bail!("expected key=value lines on stdin") };
        let k = k.trim();
        if !valid_key(k) {
            anyhow::bail!("not a plain key: {k:?}");
        }
        out.push((k.to_string(), v.to_string()));
    }
    if out.is_empty() {
        anyhow::bail!("nothing on stdin (key=value lines)");
    }
    Ok(out)
}

/// One line from the terminal with echo off (as getpass).
fn read_hidden(prompt: &str) -> anyhow::Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let fd = libc::STDIN_FILENO;
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    let tty = unsafe { libc::tcgetattr(fd, &mut t) } == 0;
    let old = t;
    if tty {
        t.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
    }
    let mut s = String::new();
    let r = std::io::stdin().lock().read_line(&mut s);
    if tty {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
        println!();
    }
    r?;
    Ok(s.trim_end_matches(['\n', '\r']).to_string())
}

fn group_id(name: &str) -> Option<u32> {
    std::fs::read_to_string("/etc/group")
        .ok()?
        .lines()
        .find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.first() == Some(&name)).then(|| f.get(2)?.parse().ok()).flatten()
        })
}

fn port_open(host: &str, port: u16) -> bool {
    (host, port).to_socket_addrs().ok().and_then(|mut a| a.next()).is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
}

async fn victron_pair(host: &str, role: &str) -> anyhow::Result<()> {
    let device_id = format!("aetherrv{}", hostname().to_lowercase().replace('-', ""));
    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(8)).build()?;
    println!("Waiting for pairing mode on {host} (device_id={device_id}, role={role}). Enable it on the GX now…");
    let deadline = Instant::now() + Duration::from_secs(240);
    let url = format!("https://{host}/auth/generate-token/");
    while Instant::now() < deadline {
        let body = format!("role={}&device_id={}", form(role), form(&device_id));
        match http.post(&url).header("content-type", "application/x-www-form-urlencoded").body(body).send().await {
            Ok(r) if r.status().as_u16() == 200 => {
                let j: serde_json::Value = r.json().await?;
                let (Some(user), Some(pw)) = (j["token_name"].as_str(), j["password"].as_str()) else {
                    anyhow::bail!("the GX answered without token_name/password");
                };
                store(SECRETS, &[("victron_mqtt_user".into(), user.into()), ("victron_mqtt_password".into(), pw.into())])?;
                println!("Paired. MQTT username = {user} (password stored in {SECRETS}, 0640)");
                for p in [1883, 8883] {
                    println!("  MQTT port {p}: {}", if port_open(host, p) { "open" } else { "closed" });
                }
                return Ok(());
            }
            Ok(r) => {
                let s = r.status().as_u16();
                let t = r.text().await.unwrap_or_default();
                println!("  not yet: HTTP {s} {}", t.chars().take(120).collect::<String>().trim());
            }
            Err(e) => println!("  not yet: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    anyhow::bail!("Timed out: pairing mode was not detected within 4 minutes.")
}

/// application/x-www-form-urlencoded value.
fn form(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The Peplink router API (`https://<router>/api`), session cookie kept by hand: the router sets it for a bare IP
/// address, which cookie jars tend to drop.
struct Router {
    http: reqwest::Client,
    base: String,
    cookies: Mutex<BTreeMap<String, String>>,
}

impl Router {
    fn new(base: &str) -> anyhow::Result<Router> {
        let http = reqwest::Client::builder().danger_accept_invalid_certs(true).timeout(Duration::from_secs(15)).build()?;
        Ok(Router { http, base: base.trim_end_matches('/').to_string(), cookies: Mutex::new(BTreeMap::new()) })
    }

    /// POST with a JSON body, or GET without one. The router answers errors as `{"stat": "fail", …}`.
    async fn call(&self, func: &str, body: Option<Value>, query: &[(&str, &str)]) -> anyhow::Result<Value> {
        let mut url = format!("{}/{func}", self.base);
        for (i, (k, v)) in query.iter().enumerate() {
            url += &format!("{}{k}={}", if i == 0 { '?' } else { '&' }, form(v));
        }
        let mut rq = match &body {
            Some(b) => self.http.post(&url).json(b),
            None => self.http.get(&url),
        };
        let cookie = self.cookies.lock().unwrap().iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
        if !cookie.is_empty() {
            rq = rq.header("cookie", cookie);
        }
        let r = rq.send().await?;
        for sc in r.headers().get_all("set-cookie") {
            let Ok(sc) = sc.to_str() else { continue };
            let first = sc.split(';').next().unwrap_or("");
            let (k, v) = first.split_once('=').unwrap_or((first, ""));
            if !k.trim().is_empty() {
                self.cookies.lock().unwrap().insert(k.trim().into(), v.trim().into());
            }
        }
        let status = r.status();
        let bytes = r.bytes().await?;
        if bytes.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("{func}: HTTP {status}, not JSON"))
    }

    async fn logout(&self) {
        let _ = self.call("logout", Some(json!({})), &[]).await;
    }
}

fn stat_msg(r: &Value) -> String {
    format!("{} {}", r["code"], r["message"].as_str().unwrap_or(""))
}

/// Log in as the router's admin, create the read-write API client, prove it gets a token, `save` (id, secret), and
/// return the router's WAN priority list read with that token. Always logs out.
async fn peplink_rw_client(router: &Router, user: &str, password: String, save: impl FnOnce(&str, &str) -> anyhow::Result<()>) -> anyhow::Result<Value> {
    let r = router.call("login", Some(json!({"username": user, "password": password})), &[]).await?;
    drop(password);
    if r["stat"] != "ok" {
        anyhow::bail!("Login failed: {}", stat_msg(&r));
    }
    let post = &r["response"]["permission"]["POST"];
    if matches!(post, Value::Null | Value::Bool(false)) || *post == json!(0) {
        router.logout().await;
        anyhow::bail!("That login has no write permission; use the router's admin account.");
    }
    let out = create_rw_client(router, save).await;
    router.logout().await;
    out
}

async fn create_rw_client(router: &Router, save: impl FnOnce(&str, &str) -> anyhow::Result<()>) -> anyhow::Result<Value> {
    let chk = router.call("status.wan.connection", None, &[]).await?; // a harmless read: is the session carried?
    if chk["stat"] != "ok" {
        let seen: Vec<String> = router.cookies.lock().unwrap().keys().cloned().collect();
        anyhow::bail!("Logged in, but the session isn't accepted ({}; cookies seen: {seen:?}). Nothing created.", stat_msg(&chk));
    }
    println!("Session check: OK.");
    let r = router.call("auth.client", Some(json!({"action": "add", "name": RW_CLIENT_NAME, "scope": "api"})), &[]).await?;
    if r["stat"] != "ok" {
        anyhow::bail!("Creating the API client failed: {}", stat_msg(&r));
    }
    let (Some(cid), Some(csec)) = (r["response"]["clientId"].as_str(), r["response"]["clientSecret"].as_str()) else {
        let fields: Vec<&String> = r["response"].as_object().map(|o| o.keys().collect()).unwrap_or_default();
        anyhow::bail!("Unexpected reply (fields: {fields:?}); nothing written.");
    };
    println!("Created API client \"{RW_CLIENT_NAME}\" (read-write).");
    let g = router.call("auth.token.grant", Some(json!({"clientId": cid, "clientSecret": csec, "scope": "api"})), &[]).await?;
    if g["stat"] != "ok" {
        anyhow::bail!("The new client can't get a token: {}; nothing written.", stat_msg(&g));
    }
    let Some(tok) = g["response"]["accessToken"].as_str() else { anyhow::bail!("The token reply has no accessToken; nothing written.") };
    println!("Token check: OK.");
    save(cid, csec)?;
    let p = router.call("config.wan.connection.priority", None, &[("accessToken", tok)]).await?;
    // the token would expire on its own; a failed revoke changes nothing
    let _ = router.call("auth.token.revoke", Some(json!({"clientId": cid, "accessToken": tok})), &[]).await;
    Ok(p.get("response").cloned().unwrap_or(p))
}

const USAGE: &str = "usage: aetherrv-setup victron-pair <gx-address> [role]\n       aetherrv-setup peplink-rw-client <router-address>\n       aetherrv-setup store-secrets   (key=value lines on stdin)";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("victron-pair") => {
            let host = a.get(1).ok_or_else(|| anyhow::anyhow!("usage: aetherrv-setup victron-pair <gx-address> [role]"))?;
            need_root()?;
            victron_pair(host, a.get(2).map(String::as_str).unwrap_or("aetherrv")).await
        }
        Some("peplink-rw-client") => {
            let host = a.get(1).ok_or_else(|| anyhow::anyhow!("usage: aetherrv-setup peplink-rw-client <router-address>"))?;
            need_root()?;
            println!("Peplink router {host}: create the read-write API client for AetherRV's WAN priority control.");
            print!("Router admin username: ");
            std::io::stdout().flush()?;
            let mut user = String::new();
            std::io::stdin().lock().read_line(&mut user)?;
            let pw = read_hidden("Router admin password (not shown): ")?;
            let router = Router::new(&format!("https://{host}/api"))?;
            let prio = peplink_rw_client(&router, user.trim(), pw, |id, secret| {
                store(SECRETS, &[("peplink_rw_client_id".into(), id.into()), ("peplink_rw_client_secret".into(), secret.into())])?;
                println!("Saved to {SECRETS} (the previous file kept as {SECRETS}.bak).");
                Ok(())
            })
            .await?;
            println!("\nRouter's WAN priority list (for the format check):");
            println!("{}", serde_json::to_string_pretty(&prio)?.chars().take(2000).collect::<String>());
            println!("\nDone.");
            Ok(())
        }
        Some("store-secrets") => {
            need_root()?;
            let pairs = read_pairs(std::io::stdin().lock())?;
            store(SECRETS, &pairs)?;
            println!("Stored {} in {SECRETS} (root:aetherrv 0640).", pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", "));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aetherrv_sim::peplink::{PeplinkSim, PeplinkSimConfig};

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_write_replaces_only_its_keys_and_keeps_the_rest() {
        let old = "# AetherRV secrets\npeplink_client_id = \"x\"\nvictron_mqtt_user = \"old\"\nvictron_mqtt_user_note = \"kept\"\n";
        let p = pairs(&[("victron_mqtt_user", "u"), ("victron_mqtt_password", "p\"w")]);
        let t = merged(old, &p);
        assert_eq!(
            t,
            "# AetherRV secrets\npeplink_client_id = \"x\"\nvictron_mqtt_user_note = \"kept\"\nvictron_mqtt_user = \"u\"\nvictron_mqtt_password = \"p\\\"w\"\n"
        );
        check(&t, &p).unwrap();
        assert_eq!(form("a b&c"), "a+b%26c");
    }

    #[test]
    fn awkward_values_read_back_exactly() {
        let p = pairs(&[("k", "q\"uo\\te ' = # [x] \t \u{1F600} \u{7f} \u{1b} \r\n")]);
        check(&merged("", &p), &p).unwrap();
    }

    #[test]
    fn a_file_the_services_cannot_read_is_refused() {
        let p = pairs(&[("tomtom_api_key", "k")]);
        assert!(check(&merged("broken = \n", &p), &p).is_err());
        // appended after a table, the key would belong to the table
        assert!(check(&merged("[x]\na = \"1\"\n", &p), &p).is_err());
        // the same key twice
        assert!(check(&merged("", &pairs(&[("a", "1"), ("a", "2")])), &pairs(&[("a", "1")])).is_err());
    }

    #[test]
    fn stdin_pairs() {
        assert_eq!(read_pairs("a=b=c\n\nk2= v \n".as_bytes()).unwrap(), pairs(&[("a", "b=c"), ("k2", " v ")]));
        assert!(read_pairs("just-a-secret\n".as_bytes()).is_err());
        assert!(read_pairs("bad key=1\n".as_bytes()).is_err());
        assert!(read_pairs("\n".as_bytes()).is_err());
    }

    #[test]
    fn store_writes_root_aetherrv_0640_and_keeps_a_backup() {
        if unsafe { libc::geteuid() } != 0 || group_id("aetherrv").is_none() {
            return; // chown needs root and the group; merged() and check() are covered above
        }
        let d = std::env::temp_dir().join(format!("setup-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("secrets.toml");
        std::fs::write(&p, "peplink_client_id = \"x\"\n").unwrap();
        store(p.to_str().unwrap(), &pairs(&[("tomtom_api_key", "k")])).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "peplink_client_id = \"x\"\ntomtom_api_key = \"k\"\n");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(std::fs::read_to_string(d.join("secrets.toml.bak")).unwrap(), "peplink_client_id = \"x\"\n");
        std::fs::remove_dir_all(&d).unwrap();
    }

    async fn sim() -> (PeplinkSim, Router) {
        let sim = PeplinkSim::start(PeplinkSimConfig::default()).await.unwrap();
        let r = Router::new(&format!("https://{}/api", sim.host())).unwrap();
        (sim, r)
    }

    #[tokio::test]
    async fn the_admin_login_creates_a_read_write_client_that_gets_a_token_then_logs_out() {
        let (sim, router) = sim().await;
        let saved = Mutex::new(None);
        let prio = peplink_rw_client(&router, "admin", "admin-pass".into(), |id, secret| {
            *saved.lock().unwrap() = Some((id.to_string(), secret.to_string()));
            Ok(())
        })
        .await
        .unwrap();
        let (id, secret) = saved.lock().unwrap().clone().expect("saved");
        sim.with(|st| {
            let c = st.api_clients.iter().find(|c| c.id == id).expect("the client exists on the router");
            assert_eq!((c.secret.as_str(), c.scope.as_str()), (secret.as_str(), "api"));
            assert!(st.sessions.is_empty(), "logged out");
            assert!(st.tokens.is_empty(), "the check token was revoked");
            assert_eq!(st.calls.get("POST logout"), Some(&1));
        });
        assert!(prio.is_object() || prio.is_array(), "the priority list: {prio}");
        sim.stop();
    }

    #[tokio::test]
    async fn a_read_only_login_creates_nothing_and_logs_out() {
        let (sim, router) = sim().await;
        let e = peplink_rw_client(&router, "viewer", "ro-pass".into(), |_, _| panic!("nothing may be saved")).await.unwrap_err();
        assert!(e.to_string().contains("no write permission"), "{e}");
        sim.with(|st| {
            assert!(st.api_clients.is_empty());
            assert!(st.sessions.is_empty(), "logged out");
        });
        sim.stop();
    }

    #[tokio::test]
    async fn a_wrong_password_creates_nothing() {
        let (sim, router) = sim().await;
        let e = peplink_rw_client(&router, "admin", "wrong".into(), |_, _| panic!("nothing may be saved")).await.unwrap_err();
        assert!(e.to_string().starts_with("Login failed: 401"), "{e}");
        sim.with(|st| assert!(st.api_clients.is_empty()));
        sim.stop();
    }

    #[tokio::test]
    async fn a_failed_save_still_logs_out() {
        let (sim, router) = sim().await;
        let e = peplink_rw_client(&router, "admin", "admin-pass".into(), |_, _| anyhow::bail!("disk full")).await.unwrap_err();
        assert_eq!(e.to_string(), "disk full");
        sim.with(|st| assert!(st.sessions.is_empty(), "logged out"));
        sim.stop();
    }
}
