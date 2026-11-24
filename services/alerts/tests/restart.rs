//! An alert that is still active when the alerts service restarts carries on: same start time, same
//! acknowledgement, no second "raised" (owner decision Q-A2, ARCHITECTURE.md §13a D19). Not in the Python tests:
//! the Python backend restarted as a whole and re-raised everything.

use aetherrv_alerts::rules::r_battery;
use aetherrv_alerts::{AlertCfg, Alerts, RESUME_WINDOW_S, Rule, open_db};
use aetherrv_core::state::StateStore;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

fn engine(db: &Path, soc: f64) -> (Arc<StateStore>, Alerts) {
    let s = Arc::new(StateStore::new());
    s.set("victron.battery.soc", json!(soc));
    let al = Alerts::new(s.clone(), open_db(Some(db)), AlertCfg::default(), Some(vec![Rule::new("battery", r_battery, 30.0, 60.0)]));
    (s, al)
}

fn events(db: &Path) -> Vec<String> {
    let c = rusqlite::Connection::open(db).unwrap();
    let mut st = c.prepare("SELECT event FROM alert_log ORDER BY t, rowid").unwrap();
    st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

fn row(al: &Alerts) -> Option<Value> {
    al.snapshot().as_array().unwrap().first().cloned()
}

#[test]
fn an_acknowledged_alert_stays_acknowledged_over_a_restart() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("h.sqlite3");
    {
        let (_s, mut al) = engine(&db, 15.0);
        al.evaluate(1000.0);
        al.evaluate(1031.0);
        assert!(al.ack("battery.soc", 1040.0));
    }
    // the new process: the condition is still there
    let (_s, mut al) = engine(&db, 15.0);
    al.evaluate(2000.0);
    let r = row(&al).expect("resumed at once, without waiting the rule's 30 s again");
    assert_eq!((r["acked"].clone(), r["since"].clone()), (json!(true), json!(1031.0)));
    al.evaluate(2000.0 + RESUME_WINDOW_S + 1.0);
    assert_eq!(events(&db), ["raised", "acked"], "no second raise, nothing cleared");
}

#[test]
fn an_unacknowledged_alert_carries_on_unacknowledged() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("h.sqlite3");
    {
        let (_s, mut al) = engine(&db, 15.0);
        al.evaluate(1000.0);
        al.evaluate(1031.0);
    }
    let (_s, mut al) = engine(&db, 15.0);
    al.evaluate(2000.0);
    let r = row(&al).unwrap();
    assert_eq!((r["acked"].clone(), r["since"].clone()), (json!(false), json!(1031.0)));
    assert_eq!(events(&db), ["raised"]);
}

#[test]
fn an_alert_gone_after_the_restart_is_closed_and_a_later_one_is_new() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("h.sqlite3");
    {
        let (_s, mut al) = engine(&db, 15.0);
        al.evaluate(1000.0);
        al.evaluate(1031.0);
        al.ack("battery.soc", 1040.0);
    }
    let (s, mut al) = engine(&db, 60.0); // charged while the service was down
    al.evaluate(2000.0);
    assert!(row(&al).is_none());
    al.evaluate(2000.0 + RESUME_WINDOW_S);
    assert_eq!(events(&db), ["raised", "acked", "cleared"]);
    // the battery drops again later: a new alert, un-acknowledged, after the rule's 30 s
    s.set("victron.battery.soc", json!(15.0));
    al.evaluate(3000.0);
    assert!(row(&al).is_none());
    al.evaluate(3031.0);
    assert_eq!(row(&al).unwrap()["acked"], json!(false));
}

#[test]
fn an_alert_that_escalated_while_down_is_raised_afresh() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("h.sqlite3");
    {
        let (_s, mut al) = engine(&db, 15.0); // warning
        al.evaluate(1000.0);
        al.evaluate(1031.0);
        al.ack("battery.soc", 1040.0);
    }
    let (_s, mut al) = engine(&db, 5.0); // now critical
    al.evaluate(2000.0);
    assert!(row(&al).is_none(), "a different severity is not resumed: it waits its 30 s like any new alert");
    al.evaluate(2031.0);
    let r = row(&al).unwrap();
    assert_eq!((r["severity"].clone(), r["acked"].clone()), (json!("critical"), json!(false)));
}
