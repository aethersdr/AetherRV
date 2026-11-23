//! Port of tests/test_rixens_http.py: the status.xml parser, the state, the command plan and the fuel
//! accounting. The command-flow tests (HTTP fake, send once, verify, gate) belong to the service.

use proto_mcs7::http::{FuelAccount, PlanError, Status, Verify, parse_status, plan, to_state};
use proto_mcs7::{FanSpeed, xml};
use serde_json::{Value, json};

fn fixture() -> String {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/mcs7_status_hw1_1.208.xml");
    std::fs::read_to_string(p).expect("fixture")
}

fn parse(x: &str) -> Status {
    parse_status(x).expect("status.xml")
}

/// Python `round(x, n)` on a float, for the expected values.
fn round(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap()
}

/// The first `<tag>(-?\d+)</tag>` of the raw XML (the Python test's `raw()`).
fn raw(xml: &str, tag: &str) -> i64 {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        if let Some(j) = rest.find(&close)
            && let Ok(v) = rest[..j].parse::<i64>()
            && !rest[..j].starts_with('+')
        {
            return v;
        }
    }
    panic!("no <{tag}>")
}

/// `re.sub(rf"<{tag}>-?\d+</{tag}>", f"<{tag}>{val}</{tag}>", x)`
fn sub_tag(xml: &str, tag: &str, val: i64) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        out.push_str(&rest[..i]);
        let body = &rest[i + open.len()..];
        match body.find(&close) {
            Some(j) if body[..j].parse::<i64>().is_ok() && !body[..j].starts_with('+') => {
                out.push_str(&format!("{open}{val}{close}"));
                rest = &body[j + close.len()..];
            }
            _ => {
                out.push_str(&open);
                rest = body;
            }
        }
    }
    out.push_str(rest);
    out
}

fn get<'a>(out: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    out.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn act_val(p: Result<Option<proto_mcs7::Plan>, PlanError>) -> (i64, i64) {
    let p = p.expect("allowed").expect("a change");
    (p.act, p.val)
}

/// py: tests/test_rixens_http.py::test_parse_live_fixture
#[test]
fn test_parse_live_fixture() {
    let fx = fixture();
    let d = parse(&fx);
    assert!(d.cabin_c == Some(7.2) && d.cabin_rh == Some(46.1) && d.setpoint_c == Some(7.2)); // owner: van ~46 °F
    assert!(d.fan_speed == Some(FanSpeed::Auto) && d.sources == ["furnace"] && d.furnace == Some("ready"));
    assert!(
        d.battery_v == Some(raw(&fx, "battv") as f64 / 10.0) && d.inlet_c == Some(raw(&fx, "inlettemp") as f64 / 100.0)
    );
    assert!(d.outlet_c == Some(raw(&fx, "outlettemp") as f64 / 100.0) && d.heater_state.as_deref() == Some("off"));
    assert_eq!(d.runtime_h, Some(round(raw(&fx, "runtime") as f64 / 60.0, 1)));
    assert!(d.fault_active.is_none() && d.fault_history.is_empty());
    assert!(d.available.electric && !d.available.aux);
}

/// py: tests/test_rixens_http.py::test_no_secrets_reach_state
#[test]
fn test_no_secrets_reach_state() {
    let fx = fixture();
    let blob = serde_json::to_string(&to_state(&parse(&fx), true, 1_795_136_523.0)).unwrap();
    assert!(!blob.contains("REDACTED") && !parse(&fx).to_map().contains_key("key"));
}

/// py: tests/test_rixens_http.py::test_sentinels_and_fan_variants
#[test]
fn test_sentinels_and_fan_variants() {
    let fx = fixture();
    let x = fx.replace("<currenttemp>72</currenttemp>", "<currenttemp>-9990</currenttemp>");
    let x = x.replace("<fanspeed>Auto</fanspeed>", "<fanspeed>40</fanspeed>");
    let d = parse(&x);
    assert!(d.cabin_c.is_none() && d.fan_speed == Some(FanSpeed::Pct(40)));
    assert_eq!(
        parse(&fx.replace("<fanspeed>Auto</fanspeed>", "<fanspeed>Off</fanspeed>")).fan_speed,
        Some(FanSpeed::Off)
    );
}

/// py: tests/test_rixens_http.py::test_cabin_source_switch
#[test]
fn test_cabin_source_switch() {
    let d = parse(&fixture());
    assert_eq!(get(&to_state(&d, true, 0.0), "climate.cabin.temp"), Some(&json!(7.2)));
    assert!(get(&to_state(&d, false, 0.0), "climate.cabin.temp").is_none());
}

/// py: tests/test_rixens_http.py::test_plan_allowlist_and_ranges
#[test]
fn test_plan_allowlist_and_ranges() {
    let cur = parse(&fixture());
    assert_eq!(plan("setpoint", &json!(7.2), &cur), Ok(None)); // no change → no send
    let p = plan("setpoint", &json!(20.0), &cur).unwrap().unwrap();
    assert_eq!((p.act, p.val), (1, 200));
    assert_eq!(plan("fan", &json!("auto"), &cur), Ok(None));
    assert_eq!(act_val(plan("fan", &json!(40), &cur)), (2, 40));
    assert_eq!(act_val(plan("electric", &json!(true), &cur)), (4, 1));
    assert_eq!(plan("furnace", &json!(true), &cur), Ok(None)); // already selected
    assert_eq!(act_val(plan("furnace", &json!(false), &cur)), (5, 0));
    for (cmd, v) in [
        ("setpoint", json!(40.0)),
        ("setpoint", json!(0.0)),
        ("fan", json!(150)),
        ("floor", json!(true)),
        ("prime", json!(1)),
        ("aux", json!(true)),
        ("wifi", json!("x")),
    ] {
        assert!(matches!(plan(cmd, &v, &cur), Err(PlanError::Rejected(_))), "{cmd} {v}");
    }
}

/// py: tests/test_rixens_http.py::test_fan_glow_fuel_scaling_matches_mcs7_ui
#[test]
fn test_fan_glow_fuel_scaling_matches_mcs7_ui() {
    let mut x = fixture();
    for (tag, val) in [
        ("burnermotor", 9257),
        ("glowpin", 655),
        ("dosingpump", 55),
        ("flametemp", 20775),
        ("inlettemp", 3550),
        ("outlettemp", 4650),
    ] {
        x = sub_tag(&x, tag, val);
    }
    let d = parse(&x);
    assert_eq!((d.fan_rpm, d.glow_w, d.fuel_hz), (Some(9257), Some(65.5), Some(5.5)));
    assert_eq!((d.flame_c, d.inlet_c, d.outlet_c), (Some(207.75), Some(35.5), Some(46.5)));
}

/// py: tests/test_rixens_http.py::test_fuel_flow_and_daily_total
#[test]
fn test_fuel_flow_and_daily_total() {
    // Python persists the total in sqlite (`counters`); here the caller's saved row is `saved`.
    fn account(a: &mut FuelAccount, saved: &mut Option<(String, f64)>, hz: Option<f64>, t: f64, day: &str) {
        if a.account(hz, t, day) {
            *saved = Some((a.day().to_string(), a.today_l()));
        }
    }
    let day = "2026-11-23"; // the local day is the caller's: the run never crosses midnight
    let mut a = FuelAccount::new(None, day);
    let mut saved: Option<(String, f64)> = None;
    assert_eq!(a.lph(Some(5.5)), Some(0.62)); // calibration point: 5.0 kW full output
    assert_eq!(round(a.lph(Some(2.4)).unwrap(), 2), 0.27); // ≈ Eberspächer low output (0.27 l/h)
    let d = Some(5.5);
    let t0 = 1_795_435_200.0; // 2026-11-23 12:00 UTC
    account(&mut a, &mut saved, d, t0, day);
    for i in 1..=720 {
        // one hour of 5 s polls at full output
        account(&mut a, &mut saved, d, t0 + 5.0 * i as f64, day);
    }
    assert_eq!(round(a.today_l(), 2), 0.62);
    let mut a2 = FuelAccount::new(None, day); // survives a restart (same day)
    let (sd, sv) = saved.clone().unwrap();
    a2.restore(&sd, sv);
    assert_eq!(round(a2.today_l(), 2), 0.62);
    account(&mut a, &mut saved, Some(5.5), t0 + 5.0 * 721.0 + 600.0, day); // a gap > 120 s is not integrated
    assert_eq!(round(a.today_l(), 2), 0.62);
}

/// py: tests/test_rixens_http.py::test_fluid_indicator_uses_glycolstate
#[test]
fn test_fluid_indicator_uses_glycolstate() {
    let fx = fixture();
    let d = parse(&fx.replace("<glycolstate>0</glycolstate>", "<glycolstate>2</glycolstate>"));
    assert!(d.glycol == Some("ready") && d.hot_tank == Some("off")); // settings/glycol stays 0 (hot tank mode off)
    assert_eq!(parse(&fx).glycol, Some("cold"));
}

/// py: tests/test_rixens_http.py::test_auto_fan_pct_from_pidspeed
#[test]
fn test_auto_fan_pct_from_pidspeed() {
    let x = fixture().replace("<pidspeed>0</pidspeed>", "<pidspeed>31</pidspeed>");
    let d = parse(&x);
    assert!(d.fan_speed == Some(FanSpeed::Auto) && d.fan_auto_pct == Some(31));
    let d = parse(&x.replace("<fanspeed>Auto</fanspeed>", "<fanspeed>40</fanspeed>"));
    assert!(d.fan_speed == Some(FanSpeed::Pct(40)) && d.fan_auto_pct.is_none());
}

/// py: tests/test_rixens_http.py::test_hot_water_and_preheat_plan
#[test]
fn test_hot_water_and_preheat_plan() {
    let mut cur = Status { hot_water: false, preheat: true, ..Status::default() };
    cur.available.preheat = true;
    let p = plan("hot_water", &json!(true), &cur).unwrap().unwrap();
    let verify = p.verify;
    assert!(
        (p.act, p.val) == (6, 1)
            && verify.check(&Status { hot_water: true, ..Status::default() })
            && !verify.check(&Status { hot_water: false, ..Status::default() })
    );
    assert_eq!(plan("hot_water", &json!(false), &cur), Ok(None)); // already off → no send
    assert_eq!(act_val(plan("preheat", &json!(false), &cur)), (7, 0));
    assert_eq!(plan("preheat", &json!(true), &cur), Ok(None));
    assert!(plan("preheat", &json!(true), &Status { preheat: false, ..Status::default() }).is_err());
    assert!(plan("floor", &json!(true), &cur).is_err()); // still not allowed
}

/// py: tests/test_rixens_http.py::test_thermostat_plan
#[test]
fn test_thermostat_plan() {
    let cur = Status { thermostat: true, ..Status::default() };
    assert_eq!(plan("thermostat", &json!(true), &cur), Ok(None)); // already on → nothing sent
    let p = plan("thermostat", &json!(false), &cur).unwrap().unwrap();
    let verify = p.verify;
    assert!(
        (p.act, p.val) == (9, 0)
            && verify.check(&Status { thermostat: false, ..Status::default() })
            && !verify.check(&Status { thermostat: true, ..Status::default() })
    );
}

// ── extra (no Python test) ───────────────────────────────────────────────────────────────────────────

/// The parsed dict and the state keys match, key for key and in order, what the Python reference printed
/// (`json.dumps(parse_status(FIXTURE))`, `json.dumps(to_state(d, True))` with time.time() = 1795136523.06).
#[test]
fn parse_and_state_match_python_reference() {
    let d = parse(&fixture());
    assert_eq!(
        serde_json::to_string(&d.to_map()).unwrap(),
        concat!(
            r#"{"version":"HW1W6-1.208 2026_06_18 RIXENS","heat_version":"1.07 2026-04-25","uptime":2057,"system_heat":false,"#,
            r#""heater_state_code":0,"heater_state":"off","cabin_c":7.2,"cabin_rh":46.1,"setpoint_c":7.2,"thermostat":true,"#,
            r#""fan_enabled":true,"fan_running":false,"fan_speed":"auto","fan_auto_pct":0,"pump_running":false,"#,
            r#""sources":["furnace"],"furnace":"ready","electric":"off","engine":"off","floor":"off","aux":"off","#,
            r#""hot_water":false,"hot_water_timer_s":null,"preheat":false,"glycol":"cold","hot_tank":"off","#,
            r#""available":{"electric":true,"engine":true,"preheat":true,"floor":false,"aux":false},"battery_v":13.1,"#,
            r#""runtime_h":3889.2,"flame_c":9.75,"inlet_c":6.25,"outlet_c":6.5,"altitude_m":928.0,"fan_rpm":0,"#,
            r#""glow_w":0.0,"fuel_hz":0.0,"fault_active":null,"fault_history":[]}"#
        )
    );
    let state: serde_json::Map<String, Value> = to_state(&d, true, 1_795_136_523.06).into_iter().collect();
    assert_eq!(
        serde_json::to_string(&state).unwrap(),
        concat!(
            r#"{"climate.rixens.version":"HW1W6-1.208 2026_06_18 RIXENS","#,
            r#""climate.rixens.heat_version":"1.07 2026-04-25","climate.rixens.uptime":2057,"#,
            r#""climate.rixens.system_heat":false,"climate.rixens.heater_state_code":0,"#,
            r#""climate.rixens.heater_state":"off","climate.rixens.cabin_c":7.2,"climate.rixens.cabin_rh":46.1,"#,
            r#""climate.rixens.setpoint_c":7.2,"climate.rixens.thermostat":true,"climate.rixens.fan_enabled":true,"#,
            r#""climate.rixens.fan_running":false,"climate.rixens.fan_speed":"auto","climate.rixens.fan_auto_pct":0,"#,
            r#""climate.rixens.pump_running":false,"climate.rixens.sources":["furnace"],"#,
            r#""climate.rixens.furnace":"ready","climate.rixens.electric":"off","climate.rixens.engine":"off","#,
            r#""climate.rixens.floor":"off","climate.rixens.aux":"off","climate.rixens.hot_water":false,"#,
            r#""climate.rixens.hot_water_timer_s":null,"climate.rixens.preheat":false,"#,
            r#""climate.rixens.glycol":"cold","climate.rixens.hot_tank":"off","climate.rixens.battery_v":13.1,"#,
            r#""climate.rixens.runtime_h":3889.2,"climate.rixens.flame_c":9.75,"climate.rixens.inlet_c":6.25,"#,
            r#""climate.rixens.outlet_c":6.5,"climate.rixens.altitude_m":928.0,"climate.rixens.fan_rpm":0,"#,
            r#""climate.rixens.glow_w":0.0,"climate.rixens.fuel_hz":0.0,"climate.rixens.fault_active":null,"#,
            r#""climate.rixens.fault_history":[],"climate.rixens.available":{"electric":true,"engine":true,"#,
            r#""preheat":true,"floor":false,"aux":false},"climate.rixens.live":true,"climate.rixens.coolant":6.5,"#,
            r#""climate.rixens.burner":false,"climate.rixens.state":"standby","climate.cabin.setpoint":7.2,"#,
            r#""climate.rixens.updated":1795136523.1,"climate.cabin.temp":7.2,"climate.cabin.humidity":46.1,"#,
            r#""climate.cabin.source":"rixens"}"#,
        )
    );
}

/// The Wi-Fi secrets never leave the reader: not in the extracted text, not in the parsed status.
#[test]
fn secrets_are_never_extracted() {
    let fx = fixture();
    assert!(fx.contains("REDACTED")); // the fixture does carry them
    let e = xml::extract(fx.as_bytes()).unwrap();
    assert!(!format!("{e:?}").contains("REDACTED"));
    assert!(!format!("{:?}", parse(&fx)).contains("REDACTED"));
}

/// Request values are read as Python reads them (expected results printed by the Python `plan()`).
#[test]
fn plan_reads_values_like_python() {
    let cur = parse(&fixture());
    // bool(value): any non-empty string is "on", including "false" and "0"
    assert_eq!(act_val(plan("electric", &json!("false"), &cur)), (4, 1));
    assert_eq!(act_val(plan("electric", &json!("0"), &cur)), (4, 1));
    assert_eq!(plan("electric", &json!(0), &cur), Ok(None));
    assert_eq!(plan("electric", &Value::Null, &cur), Ok(None));
    // float(value) / int(value)
    assert_eq!(act_val(plan("setpoint", &json!(true), &cur)), (1, 10));
    assert_eq!(act_val(plan("setpoint", &json!("20"), &cur)), (1, 200));
    assert_eq!(act_val(plan("setpoint", &json!(20.04), &cur)), (1, 200));
    assert_eq!(act_val(plan("setpoint", &json!(20.05), &cur)), (1, 200));
    assert_eq!(act_val(plan("fan", &json!(40.7), &cur)), (2, 40));
    assert_eq!(act_val(plan("fan", &json!("40"), &cur)), (2, 40));
    assert_eq!(act_val(plan("fan", &json!(true), &cur)), (2, 1));
    let bad = |cmd: &str, v: Value| plan(cmd, &v, &cur).unwrap_err();
    assert_eq!(bad("setpoint", json!("abc")), PlanError::BadValue("could not convert string to float: 'abc'".into()));
    assert_eq!(
        bad("setpoint", Value::Null),
        PlanError::BadValue("float() argument must be a string or a real number, not 'NoneType'".into())
    );
    assert_eq!(bad("fan", json!("40.0")), PlanError::BadValue("invalid literal for int() with base 10: '40.0'".into()));
    assert_eq!(bad("fan", json!("Auto")), PlanError::BadValue("invalid literal for int() with base 10: 'Auto'".into()));
    assert_eq!(
        bad("fan", Value::Null),
        PlanError::BadValue(
            "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'".into()
        )
    );
    assert_eq!(bad("setpoint", json!(1e30)), PlanError::Rejected("setpoint 1e+30 °C outside 1.0–33.0 °C".into()));
    assert_eq!(bad("floor", json!(true)), PlanError::Rejected("command 'floor' is not allowed".into()));
    assert_eq!(bad("it's", json!(1)), PlanError::Rejected("command \"it's\" is not allowed".into()));
    let mut no_electric = cur.clone();
    no_electric.available.electric = false;
    assert_eq!(
        plan("electric", &json!(false), &no_electric).unwrap_err(),
        PlanError::Rejected("electric heat is not installed/enabled on this MCS7".into())
    );
    assert!(!Verify::Setpoint(200).check(&cur));
}
