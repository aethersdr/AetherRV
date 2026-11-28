//! Service names on the bus and which service owns which state key (ARCHITECTURE.md §2, §4.5). Every service and
//! the gateway use these, so a key is always looked up under the same owner.

pub const ESPAR: &str = "espar";
pub const N2K: &str = "n2k";
pub const VICTRON: &str = "victron";
pub const BATTERIES: &str = "batteries-ble";
pub const RUUVI: &str = "ruuvi";
pub const RIXENS: &str = "rixens";
pub const STARLINK: &str = "starlink";
pub const PEPLINK: &str = "peplink";
pub const GPS: &str = "gps";
pub const NETMON: &str = "netmon";
pub const WEATHER: &str = "weather";
pub const POWER_GUARD: &str = "power-guard";
pub const DERIVE: &str = "derive";
pub const SETTINGS: &str = "settings";
pub const ALERTS: &str = "alerts";
pub const HISTORY: &str = "history";
pub const GATEWAY: &str = "gateway";

/// `aetherrv-can-aux@<bus>` runs as `can-aux-<bus>`.
pub fn can_aux(bus: &str) -> String {
    format!("can-aux-{bus}")
}

/// Every long-running service (the module-health list; `can-aux-*` instances are added from the config).
pub const SERVICES: [&str; 17] = [
    ESPAR, N2K, VICTRON, BATTERIES, RUUVI, RIXENS, STARLINK, PEPLINK, GPS, NETMON, WEATHER, POWER_GUARD, DERIVE, SETTINGS,
    ALERTS, HISTORY, GATEWAY,
];

/// Key prefix → owner. Longest prefix wins. Keys that depend on the config (`climate.cabin.*`, `bus.<name>.*`,
/// `<device>.ids` of an aux bus, `tx.*.<device>`) are resolved by [`owner_of`].
pub const PREFIXES: &[(&str, &str)] = &[
    ("climate.espar.", ESPAR),
    ("n2k.names_from_cerbo", VICTRON), // the Cerbo's VE.Can device list, published by the Victron service
    ("espar.", ESPAR),
    ("n2k.", N2K),
    ("switches.", N2K),
    ("switching.", N2K),
    ("alternator.", N2K),
    ("victron.", VICTRON),
    ("climate.cabin_front.", VICTRON),
    ("climate.cabin_rear.", VICTRON),
    ("climate.outside.", VICTRON),
    ("climate.fridge.", VICTRON),
    ("climate.freezer.", VICTRON),
    ("batteries.", BATTERIES),
    ("ruuvi.", RUUVI),
    ("climate.rixens.", RIXENS),
    ("starlink.", STARLINK),
    ("peplink.", PEPLINK),
    ("wan.", PEPLINK),
    ("location.", GPS),
    ("gps.", GPS),
    ("netmon.", NETMON),
    ("radar.", WEATHER),
    ("traffic.", WEATHER),
    ("power.", POWER_GUARD),
    ("energy.", DERIVE),
    ("settings.", SETTINGS),
    ("pi.", SETTINGS), // the Pi's own load (settings/src/host.rs)
    ("ui.", SETTINGS),
    ("alerts.", ALERTS),
    ("history.", HISTORY),
    ("svc.", GATEWAY),
];

/// The owner of a device's commands and `tx.*.<device>` keys.
pub fn device_owner(device: &str) -> Option<&'static str> {
    Some(match device {
        "espar" | "espar_diag" => ESPAR,
        "n2k" => N2K,
        "victron" => VICTRON,
        "rixens_http" | "rixens" => RIXENS,
        "starlink" => STARLINK,
        "peplink" => PEPLINK,
        _ => return None,
    })
}

/// Who publishes `key`. `cabin_source` is `[climate] cabin_source` ("victron" | "rixens" | "average");
/// `bus_owner` maps a CAN bus name to the service that owns it.
pub fn owner_of(key: &str, cabin_source: &str, bus_owner: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(rest) = key.strip_prefix("climate.cabin.") {
        let _ = rest;
        return Some(
            match cabin_source {
                "rixens" => RIXENS,
                "average" => DERIVE,
                _ => VICTRON,
            }
            .to_string(),
        );
    }
    if let Some(rest) = key.strip_prefix("bus.") {
        let bus = rest.split('.').next().unwrap_or_default();
        return bus_owner(bus);
    }
    for p in ["tx.enabled.", "tx.blocked."] {
        if let Some(dev) = key.strip_prefix(p) {
            return device_owner(dev).map(str::to_string);
        }
    }
    PREFIXES.iter().filter(|(p, _)| key.starts_with(p)).max_by_key(|(p, _)| p.len()).map(|(_, o)| o.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners() {
        let bus = |b: &str| match b {
            "hat1" => Some(ESPAR.to_string()),
            "hat0" | "n2k_cerbo" => Some(N2K.to_string()),
            other => Some(can_aux(other)),
        };
        assert_eq!(owner_of("climate.espar.state", "victron", &bus).as_deref(), Some(ESPAR));
        assert_eq!(owner_of("climate.cabin.temp", "average", &bus).as_deref(), Some(DERIVE));
        assert_eq!(owner_of("climate.cabin.temp", "victron", &bus).as_deref(), Some(VICTRON));
        assert_eq!(owner_of("climate.cabin_front.temp", "average", &bus).as_deref(), Some(VICTRON));
        assert_eq!(owner_of("climate.cabin_rear.temp", "average", &bus).as_deref(), Some(VICTRON));
        assert_eq!(owner_of("bus.hat0.rate", "x", &bus).as_deref(), Some(N2K));
        assert_eq!(owner_of("bus.fd0.rate", "x", &bus).as_deref(), Some("can-aux-fd0"));
        assert_eq!(owner_of("tx.enabled.espar_diag", "x", &bus).as_deref(), Some(ESPAR));
        assert_eq!(owner_of("wan.down_mbps", "x", &bus).as_deref(), Some(PEPLINK));
        assert_eq!(owner_of("nonsense", "x", &bus), None);
        assert_eq!(owner_of("n2k.names_from_cerbo", "x", &bus).as_deref(), Some(VICTRON));
        assert_eq!(owner_of("n2k.devices", "x", &bus).as_deref(), Some(N2K));
    }
}
