//! The single choke point for anything that changes a device.
//!
//! Every transmit (CAN frame, MQTT write, HTTP command, gRPC call that changes something) goes through
//! [`TxGate::check`]. Writes are denied unless the device has `tx_enabled = true` in the Pi's own config. That flag
//! is only set after the protocol for that exact device/firmware has been verified and the owner approved it.
//!
//! Defence in depth: the CAN interfaces are also listen-only at the kernel level unless configured otherwise
//! (systemd-networkd `ListenOnly=yes`), so a bug here still cannot put frames on a bus.

use crate::config::Cfg;
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("transmit to '{0}' is disabled")]
pub struct TxBlocked(pub String);

#[derive(Debug, Default)]
pub struct TxGate {
    enabled: BTreeMap<String, bool>,
    blocked: Mutex<BTreeMap<String, u64>>,
}

impl TxGate {
    pub fn new<I: IntoIterator<Item = (String, bool)>>(enabled: I) -> Self {
        TxGate { enabled: enabled.into_iter().collect(), blocked: Mutex::new(BTreeMap::new()) }
    }

    /// From the whole config, as Python's `build()`: one entry per `[device.<name>]` (`tx_enabled`), plus
    /// `<name>_diag` (`diag_tx`) for `kind = "espar"` devices.
    pub fn from_config(cfg: &Cfg) -> Self {
        let mut m = BTreeMap::new();
        for (dev, d) in cfg.at("device").entries() {
            m.insert(dev.clone(), d.flag("tx_enabled"));
            if d.str("kind", "") == "espar" {
                m.insert(format!("{dev}_diag"), d.flag("diag_tx"));
            }
        }
        TxGate { enabled: m, blocked: Mutex::new(BTreeMap::new()) }
    }

    /// Only these devices (a service keeps the entries for the devices it owns).
    pub fn only(&self, devices: &[&str]) -> TxGate {
        TxGate::new(devices.iter().map(|d| (d.to_string(), self.is_enabled(d))))
    }

    pub fn is_enabled(&self, device: &str) -> bool {
        self.enabled.get(device).copied().unwrap_or(false)
    }

    /// `quiet`: routine protocol housekeeping (answering a peer's address-claim poll, cyclic heater frames) is
    /// logged at DEBUG when allowed; a BLOCKED transmit is always a warning.
    pub fn check(&self, device: &str, what: &str, quiet: bool) -> Result<(), TxBlocked> {
        if self.is_enabled(device) {
            if quiet {
                tracing::debug!("TX allowed  {device}: {what}");
            } else {
                tracing::info!("TX allowed  {device}: {what}");
            }
            return Ok(());
        }
        *self.blocked.lock().unwrap().entry(device.to_string()).or_insert(0) += 1;
        tracing::warn!("TX BLOCKED {device}: {what} (tx_enabled is false)");
        Err(TxBlocked(device.to_string()))
    }

    pub fn blocked_count(&self) -> BTreeMap<String, u64> {
        self.blocked.lock().unwrap().clone()
    }

    pub fn devices(&self) -> Vec<String> {
        self.enabled.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_by_default_and_count_blocks() {
        let g = TxGate::new([("n2k".to_string(), true)]);
        assert!(g.check("n2k", "x", false).is_ok());
        assert_eq!(g.check("espar", "x", false), Err(TxBlocked("espar".into())));
        assert!(g.check("espar", "y", true).is_err());
        assert_eq!(g.blocked_count().get("espar"), Some(&2));
        assert!(!g.is_enabled("unknown"));
    }

    #[test]
    fn from_config_adds_espar_diag() {
        let c = Cfg::parse(
            "[device.espar]\nkind='espar'\ntx_enabled=true\ndiag_tx=true\n[device.n2k]\nkind='n2k'\n[device.x]\ntx_enabled='yes'",
        )
        .unwrap();
        let g = TxGate::from_config(&c);
        assert!(g.is_enabled("espar") && g.is_enabled("espar_diag"));
        assert!(!g.is_enabled("n2k"));
        assert!(!g.is_enabled("x"), "only a TOML boolean enables transmit");
        assert!(!g.is_enabled("n2k_diag"));
    }
}
