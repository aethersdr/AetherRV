//! Ported from `tests/test_victron.py`: temperatures (Ruuvi tags by type, name or instance), the battery
//! collection, and re-subscribing after a reconnect.

use aetherrv_core::cmdlog::CommandLog;
use aetherrv_core::state::{StateExt, StateStore, StateView};
use aetherrv_core::txgate::TxGate;
use aetherrv_victron::model::{Want, roles};
use aetherrv_victron::{Link, Victron};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// `VictronAdapter(s, "x", cabin_temp=…, outside_temp=…, fridge_temp=…, freezer_temp=…)` (cabin_role "cabin").
fn adapter(cabin: Want, outside: Want, fridge: Want, freezer: Want) -> Arc<Victron> {
    Victron::new(
        Arc::new(StateStore::new()),
        TxGate::new([]),
        CommandLog::memory("victron"),
        roles("cabin", cabin, outside, fridge, freezer),
    )
}

fn by(v: Value) -> Want {
    Want::from_value(Some(&v))
}

/// Python `feed(a, inst, **fields)`: the fields in keyword order.
fn feed(a: &Victron, inst: &str, fields: &[(&str, Value)]) {
    for (k, v) in fields {
        a.on_temp(&format!("temperature/{inst}/{k}"), v.clone());
    }
}

/// py: tests/test_victron.py::test_temps_auto_pick_by_type
#[test]
fn test_temps_auto_pick_by_type() {
    let a = adapter(Want::Auto, Want::Auto, Want::Auto, Want::Auto);
    feed(&a, "20", &[("TemperatureType", json!(0)), ("Temperature", json!(12.0))]); // battery
    feed(
        &a,
        "21",
        &[("TemperatureType", json!(3)), ("Temperature", json!(19.5)), ("Humidity", json!(41.0)), ("CustomName", json!("Cabin"))],
    );
    feed(&a, "22", &[("TemperatureType", json!(4)), ("Temperature", json!(-3.0))]);
    let s = &a.state;
    assert_eq!(s.get("climate.cabin.temp"), Some(json!(19.5)));
    assert_eq!(s.get("climate.cabin.humidity"), Some(json!(41.0)));
    assert_eq!(s.get("climate.outside.temp"), Some(json!(-3.0)));
    assert_eq!(s.list("victron.temps").len(), 3);
}

/// py: tests/test_victron.py::test_temps_explicit_by_name_or_instance
#[test]
fn test_temps_explicit_by_name_or_instance() {
    let a = adapter(by(json!("Galley")), by(json!(30)), Want::Auto, Want::Auto);
    feed(&a, "29", &[("TemperatureType", json!(3)), ("Temperature", json!(25.0))]); // a "room" sensor that is NOT the chosen one
    feed(&a, "31", &[("CustomName", json!("Galley")), ("Temperature", json!(21.0))]);
    feed(&a, "30", &[("Temperature", json!(5.0))]);
    assert_eq!(a.state.get("climate.cabin.temp"), Some(json!(21.0)));
    assert_eq!(a.state.get("climate.outside.temp"), Some(json!(5.0)));
}

/// py: tests/test_victron.py::test_four_ruuvi_tags_by_name
#[test]
fn test_four_ruuvi_tags_by_name() {
    let a = adapter(by(json!("Inside Temp")), by(json!("Outside Temp")), by(json!("Fridge Temp")), by(json!("freezer temp")));
    for (inst, name, temp) in
        [("20", "Inside Temp", 20.5), ("21", "Outside Temp", -1.0), ("22", "Fridge Temp", 3.5), ("23", "Freezer Temp", -18.0)]
    {
        // Ruuvi as "generic"
        feed(&a, inst, &[("TemperatureType", json!(2)), ("CustomName", json!(name)), ("Temperature", json!(temp))]);
    }
    feed(&a, "20", &[("Humidity", json!(44.0))]);
    let got: Vec<Option<Value>> =
        ["cabin", "outside", "fridge", "freezer"].iter().map(|r| a.state.get(&format!("climate.{r}.temp"))).collect();
    assert_eq!(got, vec![Some(json!(20.5)), Some(json!(-1.0)), Some(json!(3.5)), Some(json!(-18.0))]);
    assert_eq!(a.state.get("climate.cabin.humidity"), Some(json!(44.0)));
}

/// py: tests/test_victron.py::test_battery_collection_and_kind
#[test]
fn test_battery_collection_and_kind() {
    let a = adapter(Want::Auto, Want::Auto, Want::Auto, Want::Auto);
    for (path, v) in [
        ("battery/512/System/NrOfModulesOnline", json!(1)),
        ("battery/512/Info/MaxChargeCurrent", json!(230.0)),
        ("battery/512/Alarms/CellImbalance", json!(0)),
        ("battery/512/Alarms/LowVoltage", json!(1)),
        ("battery/0/ProductName", json!("Lynx Shunt 1000A VE.Can")),
        ("battery/0/Soc", json!(85.8)),
        ("battery/0/Some/Other/Path", json!(5)),
    ] {
        a.on_battery(path, v);
    }
    let bl: HashMap<String, Value> =
        a.batteries().into_iter().map(|b| (b["instance"].as_str().unwrap().to_string(), b)).collect();
    assert_eq!(bl["512"]["kind"], "bms");
    assert_eq!(bl["512"]["active_alarms"], json!({"LowVoltage": 1}));
    assert_eq!(bl["0"]["kind"], "monitor");
    assert_eq!(bl["0"]["Soc"], json!(85.8));
    assert!(bl["0"].get("Some/Other/Path").is_none());
    // sorted by numeric instance, like Python's sorted(..., key=int)
    let order: Vec<String> = a.batteries().iter().map(|b| b["instance"].as_str().unwrap().to_string()).collect();
    assert_eq!(order, ["0", "512"]);
}

/// Records subscriptions; publishes are dropped (Python's class `C`).
#[derive(Default)]
struct Subs(Mutex<Vec<String>>);

impl Link for Subs {
    fn subscribe(&self, filter: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(filter.to_string());
        Ok(())
    }
    fn publish(&self, _topic: &str, _payload: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

/// py: tests/test_victron.py::test_resubscribes_after_reconnect
/// Clean-session MQTT: after a reconnect the data topics must be subscribed again (they were not → no data).
#[test]
fn test_resubscribes_after_reconnect() {
    let a = adapter(Want::Auto, Want::Auto, Want::Auto, Want::Auto);
    for _ in 0..2 {
        // first connection, then a reconnect
        let c = Subs::default();
        a.set_subscribed(false); // what run() does on each connect
        a.on_message(&c, "N/abc123/system/0/Serial", br#"{"value": "abc123"}"#);
        assert!(c.0.lock().unwrap().contains(&"N/abc123/battery/#".to_string()));
    }
    let c = Subs::default();
    a.set_subscribed(false); // a different Cerbo on the same address is ignored
    a.on_message(&c, "N/zzz999/system/0/Serial", br#"{"value": "zzz999"}"#);
    assert!(c.0.lock().unwrap().is_empty());
    assert_eq!(a.state.get("victron.portal"), Some(json!("abc123")));
}

/// py: tests/test_victron.py::test_configured_sensor_missing_is_not_replaced_by_type_fallback
/// A freezer Ruuvi the Cerbo types as "fridge" must not stand in for the (missing) configured fridge tag.
#[test]
fn test_configured_sensor_missing_is_not_replaced_by_type_fallback() {
    let a = adapter(Want::Auto, Want::Auto, by(json!("Fridge Temp")), by(json!("FRZ")));
    feed(&a, "22", &[("TemperatureType", json!(5)), ("CustomName", json!("FRZ")), ("Temperature", json!(-17.0))]);
    assert_eq!(a.state.get("climate.freezer.temp"), Some(json!(-17.0)));
    // Python `s.get(...) is None`: the key is set to None
    assert_eq!(a.state.get("climate.fridge.temp"), Some(Value::Null));
}

/// The role owned by another source (`cabin_source = "rixens"`: Python `cabin_temp=False`) is never written.
#[test]
fn cabin_owned_elsewhere_is_not_published() {
    let a = adapter(Want::Off, Want::Auto, Want::Auto, Want::Auto);
    feed(&a, "21", &[("TemperatureType", json!(3)), ("Temperature", json!(19.5))]);
    assert_eq!(a.state.get("climate.cabin.temp"), None);
    assert_eq!(a.state.age("victron.temps").map(|x| x < 1.0), Some(true));
}

/// Python `build()`: `cabin_temp` only with `cabin_source` "victron" (default) or "average"; with "average" the
/// role is `cabin_front` (aetherrv-derive publishes the mean); a TOML integer instance matches as its `str()`.
#[test]
fn settings_cabin_role_and_wants_from_config() {
    use aetherrv_victron::Settings;
    let dev = "[device.cerbo]\nkind = \"victron\"\nhost = \"192.0.2.10\"\ncabin_temp = \"IAT\"\noutside_temp = 30\n";
    let roles = |climate: &str| {
        let c = aetherrv_core::config::Cfg::parse(&format!("{dev}{climate}")).unwrap();
        let s = Settings::from_config(&c, &aetherrv_core::config::Cfg::empty()).unwrap().unwrap();
        assert_eq!((s.host.as_str(), s.port, s.tls, s.username.clone()), ("192.0.2.10", 1883, false, None));
        s.roles.iter().map(|r| (r.name.clone(), r.want.clone())).collect::<Vec<_>>()
    };
    let iat = Want::Match("iat".into());
    for climate in ["", "[climate]\ncabin_source = \"victron\"\n"] {
        let r = roles(climate);
        assert_eq!(r[0], ("cabin".to_string(), iat.clone()));
        assert_eq!(r[1], ("outside".to_string(), Want::Match("30".into())));
        assert_eq!(r[2], ("fridge".to_string(), Want::Auto));
        assert_eq!(r[3], ("freezer".to_string(), Want::Auto));
    }
    assert_eq!(roles("[climate]\ncabin_source = \"average\"\n")[0], ("cabin_front".to_string(), iat));
    assert_eq!(roles("[climate]\ncabin_source = \"rixens\"\n")[0], ("cabin".to_string(), Want::Off));
    // no victron device → None; no host → an error (Python: KeyError at start)
    let none = aetherrv_core::config::Cfg::parse("[device.x]\nkind = \"n2k\"").unwrap();
    assert!(Settings::from_config(&none, &aetherrv_core::config::Cfg::empty()).unwrap().is_none());
    let nohost = aetherrv_core::config::Cfg::parse("[device.v]\nkind = \"victron\"").unwrap();
    assert!(Settings::from_config(&nohost, &aetherrv_core::config::Cfg::empty()).is_err());
    // credentials come from the secrets file only; an empty user means none
    let sec = aetherrv_core::config::Cfg::parse("victron_mqtt_user = \"token/x\"\nvictron_mqtt_password = \"pw\"").unwrap();
    let s = Settings::from_config(&aetherrv_core::config::Cfg::parse(dev).unwrap(), &sec).unwrap().unwrap();
    assert_eq!((s.username.as_deref(), s.password.as_deref()), (Some("token/x"), Some("pw")));
}
