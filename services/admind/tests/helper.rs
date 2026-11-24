//! The root helper (tests/test_settings.py's helper tests ported).

use aetherrv_admind::{Bad, Helper, Sys, handle_line, wpa_psk};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default, Clone)]
struct Fake {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    objs: Arc<Mutex<Option<Value>>>,
    connect_rc: i32,
    qu: Arc<Mutex<String>>,
    must_not_run: bool,
}

impl Sys for Fake {
    fn run(&self, argv: &[&str], _t: Duration) -> (i32, String, String) {
        assert!(!self.must_not_run, "must not run: {argv:?}");
        self.calls.lock().unwrap().push(argv.iter().map(|s| s.to_string()).collect());
        if argv.len() >= 2 && argv[0] == "pacman" && argv[1] == "-Qu" {
            return (0, self.qu.lock().unwrap().clone(), String::new());
        }
        if argv.last() == Some(&"Connect") {
            return (self.connect_rc, String::new(), if self.connect_rc != 0 { "Call failed: net.connman.iwd.Failed".into() } else { String::new() });
        }
        (0, String::new(), String::new())
    }
    fn sleep(&self, _d: Duration) {}
    fn iwd_objects(&self) -> Result<Value, Bad> {
        self.objs.lock().unwrap().clone().ok_or_else(|| Bad("Wi-Fi service (iwd) isn't running".into()))
    }
}

fn helper(f: &Fake) -> Helper {
    Helper::new(Box::new(f.clone()))
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn err(r: Option<Result<Map<String, Value>, Bad>>) -> bool {
    matches!(r, Some(Err(_)))
}

/// py: tests/test_settings.py::test_helper_rejects_bad_hostnames_and_zones
#[test]
fn test_helper_rejects_bad_hostnames_and_zones() {
    let f = Fake::default();
    let h = helper(&f);
    for b in ["", "-x", "a b", "x;reboot", "$(id)", &"a".repeat(70), "bad_name"] {
        assert!(err(h.verb("set_hostname", &args(json!({"name": b})))), "{b}");
    }
    for b in ["", "../../etc/passwd", "America/../../etc", "Not/AZone", "UTC;id", "/etc/localtime"] {
        assert!(err(h.verb("set_timezone", &args(json!({"tz": b})))), "{b}");
    }
    assert!(f.calls.lock().unwrap().is_empty(), "nothing executed");
    h.verb("set_hostname", &args(json!({"name": "testpi"}))).unwrap().unwrap();
    h.verb("set_timezone", &args(json!({"tz": "America/Los_Angeles"}))).unwrap().unwrap();
    assert_eq!(*f.calls.lock().unwrap(), vec![vec!["hostnamectl", "set-hostname", "testpi"], vec!["timedatectl", "set-timezone", "America/Los_Angeles"]]);
}

/// py: tests/test_settings.py::test_helper_unknown_verb_and_peer_check
#[test]
fn test_helper_unknown_verb_and_peer_check() {
    let f = Fake::default();
    let mut h = helper(&f);
    // the peer check over a real Unix socket: the test user is NOT allowed
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("s.sock");
    let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
    h.allowed_users = vec!["nobody-at-all".into()];
    let h = Arc::new(h);
    let ask = |h: Arc<Helper>, body: &str| {
        let p2 = path.clone();
        let body = body.to_string();
        let t = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let mut c = std::os::unix::net::UnixStream::connect(p2).unwrap();
            c.write_all(format!("{body}\n").as_bytes()).unwrap();
            let mut line = String::new();
            std::io::BufReader::new(c).read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        });
        let (s, _) = l.accept().unwrap();
        aetherrv_admind::serve_conn(&h, s);
        t.join().unwrap()
    };
    let r = ask(h.clone(), r#"{"verb": "reboot"}"#);
    if unsafe { libc::getuid() } != 0 {
        assert_eq!(r, json!({"ok": false, "error": "not allowed"}));
    }
    assert!(f.calls.lock().unwrap().is_empty());
    // an allowed caller and an unknown verb
    let me = aetherrv_admind::user_name(unsafe { libc::getuid() }).unwrap();
    assert_eq!(handle_line(&h, Some(&me), br#"{"verb": "rm_rf", "args": {}}"#), json!({"ok": false, "error": "unknown verb"}));
    assert_eq!(handle_line(&h, Some(&me), br#"{"verb": "reboot", "args": [1]}"#), json!({"ok": false, "error": "unknown verb"}));
}

fn iwd_objs() -> Value {
    json!({
        "/net/connman/iwd/0/3": {"net.connman.iwd.Device": {"Name": {"data": "wld0"}, "Powered": {"data": true}},
                                 "net.connman.iwd.Station": {"State": {"data": "disconnected"}}},
        "/net/connman/iwd/0/3/4e6574_psk": {"net.connman.iwd.Network": {"Name": {"data": "Van Wifi"}, "Type": {"data": "psk"}}}
    })
}

fn fake_iwd(dir: &std::path::Path, connect_rc: i32) -> (Fake, Helper) {
    let f = Fake { connect_rc, ..Default::default() };
    *f.objs.lock().unwrap() = Some(iwd_objs());
    let mut h = helper(&f);
    h.iwd_dir = dir.to_path_buf();
    (f, h)
}

/// py: tests/test_settings.py::test_wifi_psk_vector_and_file
#[test]
fn test_wifi_psk_vector_and_file() {
    // IEEE 802.11i-2004 Annex H.4 test vector
    assert_eq!(wpa_psk("password", "IEEE"), "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
    let d = tempfile::tempdir().unwrap();
    let (f, h) = fake_iwd(d.path(), 0);
    assert!(h.iwd_file("Van Wifi", "psk").ends_with("Van Wifi.psk"));
    assert!(h.iwd_file("café!", "psk").to_string_lossy().ends_with(&format!("/={}.psk", aetherrv_admind::hex("café!".as_bytes()))));
    h.verb("wifi_connect", &args(json!({"ssid": "Van Wifi", "passphrase": "correct horse"}))).unwrap().unwrap();
    let p = d.path().join("Van Wifi.psk");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    let expect = wpa_psk("correct horse", "Van Wifi");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), format!("[Security]\nPassphrase=correct\\shorse\nPreSharedKey={expect}\n"));
    std::fs::remove_file(&p).unwrap();
    h.verb("wifi_connect", &args(json!({"ssid": "Van Wifi", "passphrase": " a\\b cdef "}))).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap().lines().nth(1).unwrap(), "Passphrase=\\sa\\\\b\\scdef\\s");
    assert!(!format!("{:?}", f.calls.lock().unwrap()).contains("correct horse"), "never on a command line");
}

/// py: tests/test_settings.py::test_wifi_bad_password_and_failed_join
#[test]
fn test_wifi_bad_password_and_failed_join() {
    let d = tempfile::tempdir().unwrap();
    let (_f, h) = fake_iwd(d.path(), 0);
    for b in ["short", &"x".repeat(64), "tab\there"] {
        assert!(err(h.verb("wifi_connect", &args(json!({"ssid": "Van Wifi", "passphrase": b})))), "{b}");
    }
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "nothing written");
    let (_f, h) = fake_iwd(d.path(), 1);
    match h.verb("wifi_connect", &args(json!({"ssid": "Van Wifi", "passphrase": "wrong password"}))) {
        Some(Err(Bad(e))) => assert!(e.contains("password"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "a wrong password isn't kept");
    assert!(err(h.verb("wifi_connect", &args(json!({"ssid": "Not Here"})))));
}

/// py: tests/test_settings.py::test_update_check_separates_held_and_never_syncs_system_db
#[test]
fn test_update_check_separates_held_and_never_syncs_system_db() {
    let d = tempfile::tempdir().unwrap();
    let f = Fake::default();
    *f.qu.lock().unwrap() =
        "chromium 158.0.1-1 -> 158.0.2-1\nsway 1:1.12-4 -> 1:1.12-5\nlinux-rpi 6.18.64-1 -> 6.18.70-1 [ignored]\n".into();
    let mut h = helper(&f);
    h.update_run = d.path().join("run");
    h.update_tmp = d.path().join("tmp");
    h.pacman_sync = d.path().join("nosync");
    let r = h.verb("update_check", &Map::new()).unwrap().unwrap();
    let names: Vec<&str> = r["updates"].as_array().unwrap().iter().map(|u| u["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["chromium", "sway"]);
    assert_eq!(r["held"], json!([{"name": "linux-rpi", "old": "6.18.64-1", "new": "6.18.70-1"}]));
    let calls = f.calls.lock().unwrap();
    let syncs: Vec<&Vec<String>> = calls.iter().filter(|c| c.iter().any(|a| a == "-Sy")).collect();
    assert!(!syncs.is_empty());
    for c in syncs {
        let i = c.iter().position(|a| a == "--dbpath").expect("--dbpath");
        assert!(!c[i + 1].contains("/var/lib/pacman"), "never the system db: {c:?}");
    }
}

/// py: tests/test_settings.py::test_helper_whois_rejects_non_tailscale_ips
#[test]
fn test_helper_whois_rejects_non_tailscale_ips() {
    let f = Fake { must_not_run: true, ..Default::default() };
    let h = helper(&f);
    for b in ["192.0.2.1", "8.8.8.8", "not-an-ip", "", "100.128.0.1"] {
        assert!(err(h.verb("tailscale_whois", &args(json!({"ip": b})))), "{b}");
    }
}

/// Every verb is in the allowlist; anything else is refused (the Python VERBS table).
#[test]
fn only_the_fixed_verbs_exist() {
    let f = Fake { must_not_run: true, ..Default::default() };
    let h = helper(&f);
    for v in ["rm", "exec", "shell", "set_password", "can_send", ""] {
        assert!(h.verb(v, &Map::new()).is_none(), "{v}");
    }
    assert!(err(h.verb("can_reset", &args(json!({"iface": "eth0"})))), "only the four CAN ports");
    assert!(err(h.verb("can_reset", &args(json!({"iface": "hat0; reboot"})))));
}
