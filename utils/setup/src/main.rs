//! `sudo aetherrv-setup victron-pair <gx-address> [role]`: pair AetherRV with a Victron GX (Venus OS >= 3.80 "MQTT
//! pairing") and store the MQTT credentials (port of tools/victron_pair.py, so a Rust-only Pi needs no Python).
//!
//! Then, on the GX: Settings → Integrations → MQTT → enable pairing mode (open for 120 s). This retries
//! `POST https://<gx>/auth/generate-token/` (form: role, device_id) every 5 s for up to 4 minutes, the request the
//! victron-mqtt library makes. On success it writes `victron_mqtt_user` / `victron_mqtt_password` into
//! /etc/aetherrv/secrets.toml (root:aetherrv 0640). The password is never printed. The GX's certificate is
//! self-signed, so it is not verified (as in the Python tool).

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const SECRETS: &str = "/etc/aetherrv/secrets.toml";

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "pi".into())
}

fn store(path: &str, user: &str, password: &str) -> anyhow::Result<()> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> =
        old.lines().filter(|l| !l.starts_with("victron_mqtt_user") && !l.starts_with("victron_mqtt_password")).map(str::to_string).collect();
    lines.push(format!("victron_mqtt_user = {}", serde_json::to_string(user)?));
    lines.push(format!("victron_mqtt_password = {}", serde_json::to_string(password)?));
    let tmp = format!("{path}.new");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all((lines.join("\n") + "\n").as_bytes())?;
        f.sync_all()?;
    }
    let gid = group_id("aetherrv").ok_or_else(|| anyhow::anyhow!("no group aetherrv"))?;
    let c = std::ffi::CString::new(tmp.clone())?;
    if unsafe { libc::chown(c.as_ptr(), 0, gid) } != 0 {
        anyhow::bail!("chown {tmp}: {}", std::io::Error::last_os_error());
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o640))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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
                store(SECRETS, user, pw)?;
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("victron-pair") => {
            let host = a.get(1).ok_or_else(|| anyhow::anyhow!("usage: aetherrv-setup victron-pair <gx-address> [role]"))?;
            victron_pair(host, a.get(2).map(String::as_str).unwrap_or("aetherrv")).await
        }
        _ => {
            eprintln!("usage: aetherrv-setup victron-pair <gx-address> [role]");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_replaces_only_the_victron_lines_and_keeps_the_rest() {
        if unsafe { libc::geteuid() } != 0 || group_id("aetherrv").is_none() {
            // chown needs root and the group; the formatting is still checked below
            assert_eq!(form("a b&c"), "a+b%26c");
            return;
        }
        let d = std::env::temp_dir().join(format!("setup-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("secrets.toml");
        std::fs::write(&p, "peplink_client_id = \"x\"\nvictron_mqtt_user = \"old\"\n").unwrap();
        store(p.to_str().unwrap(), "u", "p\"w").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t, "peplink_client_id = \"x\"\nvictron_mqtt_user = \"u\"\nvictron_mqtt_password = \"p\\\"w\"\n");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
