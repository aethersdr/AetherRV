//! The rules of `alerts.py`, one function each, plus the module-health rule (ARCHITECTURE.md §4.7).
//!
//! Each reads a [`StateView`] (the bus's view of every other service) and the [`Ctx`], and returns the findings
//! that are true right now. Debounce, escalation and the log are the engine's job ([`crate::Alerts`]).

use crate::py::{self, R, arith, cap, eq, f, fx, int, is_false, is_none, items, key, num, obj, or_str, s, truthy};
use crate::{Ctx, Finding, Rule, RuleError, RuleResult, Severity};
use aetherrv_core::names;
use aetherrv_core::state::{StateExt, StateView};
use serde_json::{Map, Value, json};

use Severity::{Critical, Serious, Warning};

fn fd(id: impl Into<String>, sev: Severity, title: impl Into<String>, detail: impl Into<String>, source: &str) -> Finding {
    Finding {
        id: id.into(),
        severity: sev,
        title: title.into(),
        detail: detail.into(),
        source: source.to_string(),
        keep_ack: false,
    }
}

/// A severity named by a device (`"warning"`, `"serious"`, `"critical"`); anything else fails the rule (Python's
/// `SEVERITY[...]` would raise later, in the engine).
fn sev_of(v: &Value) -> R<Severity> {
    v.as_str().and_then(Severity::parse).ok_or_else(|| RuleError(format!("KeyError: {v}")))
}

pub fn r_power_shutdown(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    if s_.truthy("power.heat_lockout") {
        out.push(fd(
            "power.heat_lockout",
            Serious,
            "Heat locked out: house battery low",
            "Below the heater cut-off and not charging: the Espar and MCS7 won't start (and are turned off) \
             so they can't lose power mid-burn. Lifts when charging.",
            "victron",
        ));
    }
    let h = s_.get("power.heaters_off");
    if truthy(h.as_ref()) {
        let h = obj(h.as_ref().unwrap())?;
        let soc = f(Some(key(h, "soc")?), 0)?;
        let done = match key(h, "done")? {
            Value::Array(a) => a
                .iter()
                .map(|x| x.as_str().map(str::to_string).ok_or_else(|| py::type_error("sequence item: expected str")))
                .collect::<R<Vec<String>>>()?
                .join("; "),
            Value::String(t) => t.chars().map(String::from).collect::<Vec<_>>().join("; "),
            other => return Err(py::type_error(&format!("can only join an iterable, not {other}"))),
        };
        let detail = if done.is_empty() { "Nothing was heating any more.".to_string() } else { done };
        out.push(fd("power.heaters_off", Serious, format!("Heaters turned off: house battery at {soc}%"), detail, "victron"));
    }
    let p = s_.get("power.shutdown");
    let empty = Map::new();
    let p = if truthy(p.as_ref()) { obj(p.as_ref().unwrap())? } else { &empty };
    if !is_none(p.get("in_s")) {
        out.push(fd(
            "power.shutdown",
            Critical,
            format!("Console shutting down in {} s", s(Some(key(p, "in_s")?))),
            format!(
                "House battery at {}% and not charging. AetherRV powers the Pi off cleanly at {}% before the BMS cuts \
                 power. Start charging to cancel.",
                f(Some(key(p, "soc")?), 0)?,
                f(Some(key(p, "shutdown_soc")?), 0)?
            ),
            "victron",
        ));
    } else if truthy(p.get("warning")) {
        out.push(fd(
            "power.shutdown",
            Serious,
            format!("Console will shut down at {}%", f(Some(key(p, "shutdown_soc")?), 0)?),
            format!("House battery at {}% and not charging.", f(Some(key(p, "soc")?), 0)?),
            "victron",
        ));
    }
    Ok(out)
}

pub fn r_battery(s_: &dyn StateView, c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    let c = &c.cfg;
    if let Some(soc) = s_.num("victron.battery.soc") {
        if soc <= c.soc_critical {
            out.push(fd(
                "battery.soc",
                Critical,
                format!("Battery critically low: {}%", fx(soc, 0)),
                "Reduce loads or start charging now.",
                "victron",
            ));
        } else if soc <= c.soc_warning {
            out.push(fd(
                "battery.soc",
                Warning,
                format!("Battery low: {}%", fx(soc, 0)),
                format!("Below {}%.", fx(c.soc_warning, 0)),
                "victron",
            ));
        }
    }
    if let (Some(t), Some(cur)) = (s_.num("victron.battery.temp"), s_.num("victron.battery.current"))
        && cur > 1.0
        && t <= c.lfp_charge_min_c
    {
        out.push(fd(
            "battery.cold_charge",
            Serious,
            format!("Charging cold LiFePO4 ({} °C)", fx(t, 0)),
            "Charging lithium near/below freezing can damage cells; check the BMS/DVCC limits.",
            "victron",
        ));
    }
    Ok(out)
}

/// Venus BMS alarm names → words.
pub const BMS_ALARM_NAMES: &[(&str, &str)] = &[
    ("LowVoltage", "low voltage"),
    ("HighVoltage", "high voltage"),
    ("HighCellVoltage", "high cell voltage"),
    ("LowCellVoltage", "low cell voltage"),
    ("CellImbalance", "cell imbalance"),
    ("HighChargeCurrent", "high charge current"),
    ("HighDischargeCurrent", "high discharge current"),
    ("HighTemperature", "high temperature"),
    ("LowTemperature", "low temperature"),
    ("HighChargeTemperature", "high charge temperature"),
    ("LowChargeTemperature", "low charge temperature"),
    ("InternalFailure", "internal failure"),
    ("ChargeBlocked", "charge blocked"),
    ("DischargeBlocked", "discharge blocked"),
];

fn is_intlike(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(_) | Value::Bool(_)))
}

pub fn r_bms(s_: &dyn StateView, c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for b in items(s_.get("victron.batteries"))? {
        let b = obj(&b)?;
        if b.get("kind") != Some(&json!("bms")) {
            continue;
        }
        let name = if truthy(b.get("CustomName")) {
            s(b.get("CustomName"))
        } else {
            or_str(b.get("ProductName"), "Battery BMS")
        };
        let exp = c.cfg.bms_expected_modules;
        let online = b.get("System/NrOfModulesOnline");
        // Owner 2026-11-22: the batteries are read over Bluetooth; the BMS-Can count is not a fault when Bluetooth
        // is in use (a silent battery there has its own alert, r_epoch). Without Bluetooth the count still matters.
        if exp != 0.0 && is_intlike(online) && arith(online)? < exp && s_.val("batteries.ble.reporting").is_none() {
            let (n, e) = (int(online)?, exp.trunc() as i64);
            out.push(fd(
                format!("bms.modules.{}", s(Some(key(b, "instance")?))),
                Serious,
                format!("Only {n} of {e} batteries reporting"),
                format!(
                    "{name}: the BMS reports {n} module(s) online, expected {e}. Check the battery comms daisy-chain / \
                     DIP switches (and that every battery's breaker is closed)."
                ),
                "victron",
            ));
        }
        let alarms = b.get("active_alarms");
        if truthy(alarms) {
            for (k, v) in obj(alarms.unwrap())? {
                let alarm = eq(v, &json!(2));
                let words = BMS_ALARM_NAMES.iter().find(|(n, _)| n == k).map_or(k.as_str(), |(_, w)| w);
                out.push(fd(
                    format!("bms.alarm.{}.{k}", s(Some(key(b, "instance")?))),
                    if alarm { Serious } else { Warning },
                    format!("{name}: {words}"),
                    format!("BMS {} ({k}).", if alarm { "alarm" } else { "warning" }),
                    "victron",
                ));
            }
        }
        for (k, what) in [("System/NrOfModulesBlockingCharge", "charging"), ("System/NrOfModulesBlockingDischarge", "discharging")] {
            let n = b.get(k);
            if is_intlike(n) && arith(n)? > 0.0 {
                out.push(fd(
                    format!("bms.block.{}.{what}", s(Some(key(b, "instance")?))),
                    Serious,
                    format!("{name}: {} module(s) blocking {what}", int(n)?),
                    "The BMS has disabled this direction on part of the bank.",
                    "victron",
                ));
            }
        }
    }
    Ok(out)
}

pub fn r_cold(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for (role, label) in [("fridge", "Fridge"), ("freezer", "Freezer")] {
        if s_.truthy(&format!("settings.{role}_off")) {
            continue; // the owner marked it switched off (from the alert); re-armed once it's cold again
        }
        if let (Some(t), Some(mx)) = (s_.num(&format!("climate.{role}.temp")), s_.num(&format!("ui.{role}_max_c")))
            && t > mx
        {
            out.push(fd(format!("cold.{role}"), Warning, format!("{label} warm: {} °C", fx(t, 1)), format!("Above {} °C.", fx(mx, 0)), "ruuvi"));
        }
    }
    Ok(out)
}

/// A load-controller channel latched in fault (its electronic breaker tripped). Reset = switch it off.
pub fn r_switch_trip(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for w in items(s_.get("switches.list"))? {
        let w = obj(&w)?;
        if w.get("state") == Some(&json!("error")) && truthy(w.get("channel")) {
            let ch = s(Some(key(w, "channel")?));
            out.push(fd(
                format!("switch.trip.{}.{ch}", s(w.get("bank"))),
                Serious,
                format!("Breaker tripped: {}", s(w.get("name"))),
                format!(
                    "The load controller shut off channel {ch} (overcurrent or load fault) and holds it off. Reset \
                     switches it off and clears the fault; check the load and its current limit before using it again."
                ),
                "n2k",
            ));
        }
    }
    Ok(out)
}

/// A load-controller timer that did not cut its output off (AetherRV's safety net had to).
pub fn r_trigger_failsafe(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for g in items(s_.get("switching.triggers"))? {
        let g = obj(&g)?;
        let id = s(Some(key(g, "id")?));
        let r = s_.get(&format!("switching.trigger.{id}"));
        if !truthy(r.as_ref()) {
            continue;
        }
        let r = obj(r.as_ref().unwrap())?;
        if is_false(r.get("ok")) {
            let detail = match r.get("detail") {
                None => String::new(),
                Some(Value::String(t)) => t.clone(),
                Some(other) => return Err(py::type_error(&format!("can only concatenate str (not {other}) to str"))),
            };
            out.push(fd(
                format!("trigger.failsafe.{id}"),
                Serious,
                format!("{}: controller timer failed", s(Some(key(g, "name")?))),
                detail + " Check the load controller's timer configuration.",
                "n2k",
            ));
        }
    }
    Ok(out)
}

fn max_min(vals: &[Value], max: bool) -> R<f64> {
    let mut best: Option<f64> = None;
    for v in vals {
        let x = arith(Some(v))?;
        best = Some(match best {
            None => x,
            Some(b) if (max && x > b) || (!max && x < b) => x,
            Some(b) => b,
        });
    }
    best.ok_or_else(|| RuleError("ValueError: empty sequence".into()))
}

/// Individual EPOCH batteries read over Bluetooth.
pub fn r_epoch(s_: &dyn StateView, c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for b in items(s_.get("batteries.ble"))? {
        let b = obj(&b)?;
        let nm = format!("Battery {}", s(b.get("name")));
        let k = format!("epoch.{}", s(b.get("name")));
        if !truthy(b.get("ok")) {
            if !is_none(b.get("age")) || truthy(b.get("error")) {
                out.push(fd(
                    format!("{k}.silent"),
                    Warning,
                    format!("{nm} not answering over Bluetooth"),
                    format!("No good read for 10 min ({}).", or_str(b.get("error"), "no data")),
                    "epoch",
                ));
            }
            continue;
        }
        if truthy(b.get("problem_code")) {
            out.push(fd(
                format!("{k}.fault"),
                Serious,
                format!("{nm}: BMS fault"),
                format!("Problem code {}.", s(Some(key(b, "problem_code")?))),
                "epoch",
            ));
        }
        for (fet, what) in [("charge_fet", "charging"), ("discharge_fet", "discharging")] {
            if is_false(b.get(fet)) {
                out.push(fd(
                    format!("{k}.{fet}"),
                    Serious,
                    format!("{nm}: {what} switched off by its BMS"),
                    format!("The battery's BMS has opened its {what} MOSFET."),
                    "epoch",
                ));
            }
        }
        let delta = b.get("cell_delta_mv");
        let delta_v = if truthy(delta) { arith(delta)? } else { 0.0 };
        if delta_v > 50.0 {
            out.push(fd(
                format!("{k}.cells"),
                Warning,
                format!("{nm}: cells out of balance ({} mV)", s(Some(key(b, "cell_delta_mv")?))),
                "Cell voltage spread above 50 mV.",
                "epoch",
            ));
        }
        let all = items(b.get("temps").cloned())?;
        let temps: Vec<Value> = all.iter().filter(|t| !t.is_null()).cloned().collect();
        if !temps.is_empty() {
            let hot = max_min(&temps, true)?;
            if hot >= 50.0 {
                out.push(fd(
                    format!("{k}.hot"),
                    Serious,
                    format!("{nm}: hot ({} °C)", fx(hot, 0)),
                    "A cell/BMS temperature is 50 °C or more.",
                    "epoch",
                ));
            }
        }
        // T1/T2: the cell probes
        let cell_t: Vec<Value> = all.iter().take(2).filter(|t| !t.is_null()).cloned().collect();
        if !cell_t.is_empty() {
            let t = max_min(&cell_t, false)?;
            if t < c.cfg.batt_cold_c {
                out.push(fd(
                    format!("{k}.cold"),
                    Warning,
                    format!("{nm}: cells freezing ({} °F)", fx(t * 9.0 / 5.0 + 32.0, 0)),
                    "A cell probe is below 32 °F: LiFePO4 won't accept a charge. Check the battery heater.",
                    "epoch",
                ));
            }
        }
    }
    Ok(out)
}

/// Venus notification `Type`: 0 warning, 1 alarm, 2 info.
fn cerbo_severity(v: Option<&Value>) -> Option<Severity> {
    let v = v?;
    if eq(v, &json!(0)) {
        Some(Warning)
    } else if eq(v, &json!(1)) {
        Some(Serious)
    } else {
        None
    }
}

/// Mirror the Cerbo's active alarms/warnings (the ones that sound its buzzer).
pub fn r_cerbo_notifications(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for n in items(s_.get("victron.notifications"))? {
        let n = obj(&n)?;
        let sev = cerbo_severity(n.get("type"));
        if let Some(sev) = sev.filter(|_| truthy(n.get("active"))) {
            let dev = if truthy(n.get("device")) { format!(" ({})", s(Some(key(n, "device")?))) } else { String::new() };
            out.push(fd(
                format!("cerbo.notif.{}", s(Some(key(n, "index")?))),
                sev,
                format!("Cerbo: {}{dev}", or_str(n.get("description"), "alarm")),
                if truthy(n.get("acknowledged")) {
                    "Acknowledged on the Cerbo."
                } else {
                    "Unacknowledged: the Cerbo's buzzer sounds until it is acknowledged."
                },
                "victron",
            ));
        }
    }
    Ok(out)
}

pub fn r_n2k_devices(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for d in items(s_.get("n2k.devices"))? {
        let d = obj(&d)?;
        if !truthy(d.get("online")) {
            let src = s(d.get("source"));
            let name = if truthy(d.get("model")) { s(d.get("model")) } else { format!("device {src}") };
            let age = match d.get("age") {
                None => fx(0.0, 0),
                v => f(v, 0)?,
            };
            out.push(fd(
                format!("n2k.offline.{src}"),
                Serious,
                format!("{name} offline"),
                format!("No NMEA 2000 traffic from address {src} for {age} s."),
                "n2k",
            ));
        }
    }
    Ok(out)
}

pub fn r_buses(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    let keys = s_.keys();
    let mut names: Vec<&str> =
        keys.iter().filter(|k| k.starts_with("bus.")).map(|k| k.split('.').nth(1).unwrap_or_default()).collect();
    names.sort();
    names.dedup();
    for n in names {
        if n.starts_with("bench") {
            continue;
        }
        if s_.is_false(&format!("bus.{n}.connected")) {
            out.push(fd(
                format!("bus.down.{n}"),
                Serious,
                format!("CAN bus {n} down"),
                format!("Interface {} is not up.", s(s_.get(&format!("bus.{n}.channel")).as_ref())),
                "can",
            ));
        }
    }
    for k in &keys {
        if let Some(dev) = k.strip_suffix(".path_backup")
            && s_.truthy(k)
        {
            out.push(fd(
                format!("path.backup.{dev}"),
                Warning,
                format!("{}: on the backup path ({})", dev.to_uppercase(), s(s_.get(&format!("{dev}.path")).as_ref())),
                "The Pi's own CAN connection to this bus went silent or failed to send; running through the backup. \
                 Check the CAN wiring to the Pi.",
                "can",
            ));
        }
    }
    Ok(out)
}

/// Espar heater fault code from its own status frame (read-only decoder).
pub fn r_espar(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    let lock = s_.get("climate.espar.diag.lockout");
    if truthy(lock.as_ref()) {
        let l = obj(lock.as_ref().unwrap())?;
        out.push(fd(
            "espar.lockout",
            Serious,
            format!("Espar locked out ({})", s(Some(key(l, "code")?))),
            format!("{}. {}", s(Some(key(l, "why")?)), s(Some(key(l, "help")?))),
            "espar",
        ));
    }
    for d in items(s_.get("climate.espar.diag.dtcs"))? {
        let d = obj(&d)?;
        if truthy(d.get("active")) && !truthy(d.get("lockout")) {
            let sv = d.get("severity");
            let serious = sv.is_some_and(|v| [4, 5, 6].iter().any(|x| eq(v, &json!(x))));
            let code = s(Some(key(d, "code")?));
            out.push(fd(
                format!("espar.dtc.{code}"),
                if serious { Serious } else { Warning },
                format!("Espar fault {code}"),
                or_str(d.get("text"), ""),
                "espar",
            ));
        } else if !truthy(d.get("active")) {
            // in the heater's fault memory, not active now
            let code = s(Some(key(d, "code")?));
            let text = if truthy(d.get("text")) { d.get("text").cloned().unwrap() } else { json!("unknown code") };
            let mut x = fd(
                format!("espar.stored.{code}"),
                Warning,
                format!("Espar stored fault {code}"),
                format!("{}. In the heater's fault memory, not active now. See Settings → Espar.", cap(&text)?),
                "espar",
            );
            x.keep_ack = true;
            out.push(x);
        }
    }
    let prob = s_.get("climate.espar.ctl.problem");
    if truthy(prob.as_ref()) {
        out.push(fd("espar.control", Warning, "Espar control stopped", cap(prob.as_ref().unwrap())? + ".", "espar"));
    }
    let issue = s_.get("climate.espar.issue");
    if truthy(issue.as_ref()) {
        out.push(fd("espar.issue", Warning, "Espar not heating", cap(issue.as_ref().unwrap())? + ".", "espar"));
    }
    let code = s_.get("climate.espar.fault");
    if truthy(code.as_ref()) {
        let code = s(code.as_ref());
        let sev = if s_.get("climate.espar.state") == Some(json!("error")) { Serious } else { Warning };
        out.push(fd(
            format!("espar.fault.{code}"),
            sev,
            format!("Espar heater fault {code}"),
            format!(
                "{}. Check the EasyStart Pro for the full code.",
                or_str(s_.get("climate.espar.fault_text").as_ref(), "unknown fault code")
            ),
            "espar",
        ));
    }
    Ok(out)
}

pub fn r_victron(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    if s_.is_false("victron.connected") && !s_.truthy("demo.active") {
        out.push(fd("victron.link", Serious, "Cerbo GX not connected", "No MQTT connection to the Cerbo GX.", "victron"));
    }
    Ok(out)
}

pub fn r_rixens(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    if s_.get("climate.rixens.state") == Some(json!("no data")) || s_.is_false("climate.rixens.reachable") {
        out.push(fd("rixens.link", Warning, "Rixens MCS7 not responding", "No data from the MCS7 controller.", "rixens"));
    }
    if s_.get("climate.rixens.heater_state_code").is_some_and(|v| eq(&v, &json!(200))) {
        out.push(fd(
            "rixens.error",
            Serious,
            "Furnace in error state",
            "The Eberspächer burner reports an error (state 200).",
            "rixens",
        ));
    }
    let af = s_.get("climate.rixens.fault_active").unwrap_or(Value::Null);
    if truthy(Some(&af)) {
        out.push(fd(
            "rixens.fault",
            Serious,
            format!("Furnace fault code {}", s(Some(&af))),
            "Live fault reported by the MCS7 (AF). See the Eberspächer fault table.",
            "rixens",
        ));
    }
    // dict.fromkeys: each code once, in order
    let mut seen: Vec<Value> = Vec::new();
    for code in items(s_.get("climate.rixens.fault_history"))? {
        if code.is_array() || code.is_object() {
            return Err(py::type_error("unhashable type"));
        }
        if seen.iter().any(|x| eq(x, &code)) {
            continue;
        }
        seen.push(code.clone());
        if !eq(&code, &af) {
            let mut x = fd(
                format!("rixens.stored.{}", s(Some(&code))),
                Warning,
                format!("Furnace stored fault {}", s(Some(&code))),
                "In the MCS7's fault history, not active now. See Settings → MCS7.",
                "rixens",
            );
            x.keep_ack = true;
            out.push(x);
        }
    }
    Ok(out)
}

pub fn r_starlink(s_: &dyn StateView, c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    if s_.val("starlink.reachable").is_none() {
        return Ok(out); // adapter not enabled
    }
    let rr = s_.get("starlink.restart_requested");
    let rr = if truthy(rr.as_ref()) { arith(rr.as_ref())? } else { 0.0 };
    if c.now - rr < 600.0 {
        return Ok(out); // owner-requested restart (update install): expected outage
    }
    if s_.is_false("starlink.reachable") {
        out.push(fd(
            "starlink.unreachable",
            Warning,
            "Starlink dish unreachable",
            "No gRPC response from 192.168.100.1 (powered off, stowed, or not routed by the Peplink).",
            "starlink",
        ));
        return Ok(out);
    }
    if !s_.truthy("starlink.online") {
        out.push(fd(
            "starlink.offline",
            Warning,
            format!("Starlink offline: {}", s(s_.get("starlink.state").as_ref()).replace('_', " ").to_lowercase()),
            "The dish is not connected to the constellation.",
            "starlink",
        ));
    }
    if s_.truthy("starlink.snr_persistently_low") {
        out.push(fd(
            "starlink.snr_low",
            Warning,
            "Starlink: signal persistently weak",
            format!("The dish reports its SNR as persistently low (now {} dB).", s(s_.get("starlink.snr_db").as_ref())),
            "starlink",
        ));
    }
    for a in items(s_.get("starlink.alerts"))? {
        let a = obj(&a)?;
        if truthy(a.get("severity")) {
            out.push(fd(
                format!("starlink.{}", s(Some(key(a, "id")?))),
                sev_of(key(a, "severity")?)?,
                format!("Starlink: {}", s(Some(key(a, "label")?))),
                "Reported by the dish.",
                "starlink",
            ));
        }
    }
    Ok(out)
}

pub fn r_peplink(s_: &dyn StateView, c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    if s_.val("peplink.reachable").is_none() {
        return Ok(out);
    }
    if s_.is_false("peplink.reachable") {
        out.push(fd("peplink.unreachable", Serious, "Router API unreachable", "No response from the Peplink Router API.", "peplink"));
    }
    for a in items(s_.get("peplink.allowance"))? {
        let a = obj(&a)?;
        let Some(pct) = num(a.get("percent")) else { continue };
        let sim = if truthy(a.get("sim")) { format!(" SIM {}", s(Some(key(a, "sim")?))) } else { String::new() };
        let sev = if pct >= c.cfg.data_serious_pct {
            Serious
        } else if pct >= c.cfg.data_warning_pct {
            Warning
        } else {
            continue;
        };
        out.push(fd(
            format!("peplink.data.{}.{}", s(Some(key(a, "conn")?)), s(a.get("sim"))),
            sev,
            format!("Cellular data{sim} at {}%", fx(pct, 0)),
            format!("{} of {} MB used this cycle.", s(a.get("usage_mb")), s(a.get("limit_mb"))),
            "peplink",
        ));
    }
    Ok(out)
}

/// Python `_dur(sec)`: `"2 h 5 min"`, `"10 min"` (at least 1 min), `""` for None.
pub fn dur(sec: Option<&Value>) -> R<String> {
    if is_none(sec) {
        return Ok(String::new());
    }
    let v = sec.unwrap();
    if let Some(i) = v.as_i64().or_else(|| v.as_bool().map(i64::from)) {
        return Ok(if i >= 3600 {
            format!("{} h {} min", i.div_euclid(3600), i.rem_euclid(3600).div_euclid(60))
        } else {
            format!("{} min", 1.max(i.div_euclid(60)))
        });
    }
    let x = arith(Some(v))?;
    Ok(if x >= 3600.0 {
        format!("{} h {} min", py::pyf((x / 3600.0).floor()), py::pyf((x.rem_euclid(3600.0) / 60.0).floor()))
    } else {
        let m = (x / 60.0).floor();
        if m > 1.0 { format!("{} min", py::pyf(m)) } else { "1 min".to_string() }
    })
}

pub fn r_netmon(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for t in items(s_.get("netmon.targets"))? {
        let t = obj(&t)?;
        if !is_false(t.get("up")) {
            continue;
        }
        let what = if t.get("reason") == Some(&json!("service not answering")) { "service not answering" } else { "not responding" };
        let port = if truthy(t.get("tcp")) { format!(":{}", s(Some(key(t, "tcp")?))) } else { String::new() };
        let mut detail = format!("{}{port} {what} for {}.", s(Some(key(t, "host")?)), dur(t.get("down_for"))?);
        if truthy(t.get("hint")) {
            match key(t, "hint")? {
                Value::String(h) => detail += &format!(" {h}"),
                other => return Err(py::type_error(&format!("can only concatenate str (not {other}) to str"))),
            }
        }
        let sev = if truthy(t.get("severity")) { sev_of(key(t, "severity")?)? } else { Warning };
        let name = s(Some(key(t, "name")?));
        out.push(fd(format!("net.{name}"), sev, format!("{name} {what}"), detail, "network"));
    }
    Ok(out)
}

pub fn r_system(s_: &dyn StateView, _c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    if s_.truthy("history.degraded") {
        out.push(fd(
            "system.history",
            Warning,
            "Chart history not being saved",
            format!("History database unusable, running in memory: {}", s(s_.get("history.degraded").as_ref())),
            "system",
        ));
    }
    Ok(out)
}

/// Modules whose "down" alert is serious: the ones that own a heater or the power-off (§4.7).
pub const SERIOUS_MODULES: [&str; 4] = [names::ESPAR, names::N2K, names::POWER_GUARD, names::RIXENS];

/// The systemd unit of a module (`can-aux-fd0` runs as `aetherrv-can-aux@fd0`).
pub fn unit_of(module: &str) -> String {
    match module.strip_prefix("can-aux-") {
        Some(bus) => format!("aetherrv-can-aux@{bus}"),
        None => format!("aetherrv-{module}"),
    }
}

/// New (ARCHITECTURE.md §4.7): "<module> down" for every AetherRV service seen on the bus whose status is down or
/// whose heartbeat is stale (older than 6 s, the bus's own "down"); the rule holds it for 15 s before it raises.
/// Only services that have announced themselves at least once are judged (the broker keeps their retained status
/// across restarts); this service doesn't judge itself.
pub fn r_modules(_s: &dyn StateView, c: &Ctx) -> RuleResult {
    let mut out = Vec::new();
    for m in &c.modules {
        let known = names::SERVICES.contains(&m.name.as_str()) || m.name.starts_with("can-aux-");
        if !known || m.name == names::ALERTS || m.ok {
            continue;
        }
        // one text: by the time the 15 s have passed, a stopped module's heartbeat is stale too
        out.push(fd(
            format!("svc.down.{}", m.name),
            if SERIOUS_MODULES.contains(&m.name.as_str()) { Serious } else { Warning },
            format!("{} module down", m.name),
            format!(
                "No heartbeat from {} on the bus (stopped, crashed or hung). Its values show as unknown and its \
                 controls are unavailable until it is back.",
                unit_of(&m.name)
            ),
            "system",
        ));
    }
    Ok(out)
}

/// The rule set of `alerts.py` (`default_rules`), exactly.
pub fn default_rules(cfg: &crate::AlertCfg) -> Vec<Rule> {
    let mut v = default_rules_base();
    v.extend([
        Rule::new("starlink", r_starlink, cfg.starlink_offline_s, 30.0),
        Rule::new("peplink", r_peplink, 60.0, 30.0),
        Rule::new("netmon", r_netmon, 0.0, 0.0), // NetMon already debounces (fail_after)
        Rule::new("system", r_system, 0.0, 0.0),
        Rule::new("bms", r_bms, 30.0, 30.0),
    ]);
    v
}

pub fn default_rules_base() -> Vec<Rule> {
    vec![
        Rule::new("battery", r_battery, 30.0, 60.0),
        Rule::new("power_shutdown", r_power_shutdown, 0.0, 30.0),
        Rule::new("cold", r_cold, 600.0, 120.0), // 10 min: ride through door openings
        Rule::new("n2k", r_n2k_devices, 0.0, 10.0), // the 90 s offline threshold is already a debounce
        Rule::new("buses", r_buses, 10.0, 10.0),
        Rule::new("victron", r_victron, 60.0, 10.0),
        Rule::new("rixens", r_rixens, 30.0, 10.0),
        Rule::new("switch_trip", r_switch_trip, 3.0, 5.0),
        Rule::new("cerbo", r_cerbo_notifications, 0.0, 5.0),
        Rule::new("trigger", r_trigger_failsafe, 0.0, 0.0),
        Rule::new("epoch", r_epoch, 60.0, 60.0),
        Rule::new("espar", r_espar, 5.0, 30.0),
    ]
}

/// What the service runs: Python's rules plus the module-health rule (15 s down before it raises).
pub fn service_rules(cfg: &crate::AlertCfg) -> Vec<Rule> {
    let mut v = default_rules(cfg);
    v.push(Rule::new("modules", r_modules, 15.0, 5.0));
    v
}
