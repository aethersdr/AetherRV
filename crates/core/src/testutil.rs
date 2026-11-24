//! Test helpers: a private Mosquitto on a Unix socket in a temp dir (never the system broker).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Broker {
    pub dir: tempfile::TempDir,
    pub socket: PathBuf,
    child: Option<Child>,
    persistence: bool,
    acl: Option<PathBuf>,
}

impl Broker {
    /// Start a broker; `persistence` keeps retained messages across [`Broker::restart`].
    pub fn start(persistence: bool) -> Broker {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("mqtt.sock");
        let mut b = Broker { dir, socket, child: None, persistence, acl: None };
        b.spawn();
        b
    }

    /// A broker with an ACL file (e.g. packaging/etc/mosquitto.acl).
    pub fn with_acl(acl: &Path) -> Broker {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("mqtt.sock");
        let mut b = Broker { dir, socket, child: None, persistence: false, acl: Some(acl.to_path_buf()) };
        b.spawn();
        b
    }

    fn spawn(&mut self) {
        let conf = self.dir.path().join("mosquitto.conf");
        let mut text = format!(
            "listener 0 {}\nallow_anonymous true\nlog_dest none\n",
            self.socket.display()
        );
        if let Some(a) = &self.acl {
            text += &format!("acl_file {}\n", a.display());
        }
        if self.persistence {
            text += &format!("persistence true\npersistence_location {}/\nautosave_interval 1\n", self.dir.path().display());
        } else {
            text += "persistence false\n";
        }
        std::fs::write(&conf, text).unwrap();
        let _ = std::fs::remove_file(&self.socket);
        let child = Command::new(mosquitto_bin())
            .arg("-c")
            .arg(&conf)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start mosquitto (is it installed?)");
        self.child = Some(child);
        wait_for(&self.socket, Duration::from_secs(5));
    }

    /// Kill the broker (SIGKILL: no clean shutdown).
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Stop with SIGTERM (Mosquitto saves its persistence file).
    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            unsafe {
                libc::kill(c.id() as i32, libc::SIGTERM);
            }
            let _ = c.wait();
        }
    }

    pub fn restart(&mut self) {
        self.stop();
        self.spawn();
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn mosquitto_bin() -> String {
    for p in ["/usr/bin/mosquitto", "/usr/sbin/mosquitto"] {
        if Path::new(p).exists() {
            return p.to_string();
        }
    }
    "mosquitto".to_string()
}

fn wait_for(p: &Path, timeout: Duration) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if p.exists() {
            std::thread::sleep(Duration::from_millis(30));
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("broker socket {} did not appear", p.display());
}

/// Poll `cond` until true or `timeout`.
pub async fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < end {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}
