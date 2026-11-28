//! The real listener (bluer over D-Bus): one BlueZ discovery session, LE only, with duplicate data on so that BlueZ
//! signals every advertisement (`PropertiesChanged` on `ManufacturerData`), not only changed ones.
//!
//! * A device is watched once its manufacturer data holds Ruuvi's company id; others are skipped for [`RECHECK`]
//!   ([`UNKNOWN_RECHECK`] while its data isn't known yet: a device object can appear before its first advertisement).
//! * Only the `PropertiesChanged` signals count as advertisements: the values cached on a device object when the
//!   session starts may be old (BlueZ keeps devices it heard earlier), so they are never reported as heard.
//! * BlueZ drops a device it hasn't heard for its TemporaryTimeout (30 s); its watcher then ends, and the device is
//!   watched again when it is heard again.
//! * bluetoothd down or restarting ends the session: reported as `error`, retried after a backoff (2 s doubling to
//!   60 s); the process never exits for it.
//!
//! Nothing is ever written to a tag (no connection is made). Not exercised by the tests (no Bluetooth there); the
//! tracker above it is.

use crate::{Advert, Scan, norm_addr};
use aetherrv_core::clock::now_s;
use bluer::{Address, AdapterEvent, DeviceEvent, DeviceProperty, DiscoveryFilter, DiscoveryTransport, Session};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

/// How long a device that isn't a Ruuvi is skipped before it is checked again.
pub const RECHECK: Duration = Duration::from_secs(600);
/// The same for a device whose manufacturer data isn't known yet.
pub const UNKNOWN_RECHECK: Duration = Duration::from_secs(30);

type Watched = Arc<Mutex<HashSet<Address>>>;

/// Listen until the process ends; adverts go to `tx`, the state to `scan`.
pub async fn listen(tx: mpsc::Sender<Advert>, scan: watch::Sender<(Scan, f64)>) {
    let mut backoff = 2u64;
    loop {
        let _ = scan.send((Scan::Starting, now_s()));
        let began = Instant::now();
        let r = session(&tx, &scan).await;
        if began.elapsed() > Duration::from_secs(60) {
            backoff = 2; // it ran for a while: a fresh failure starts the backoff again
        }
        match r {
            Ok(()) => tracing::warn!("ruuvi: the discovery session ended; starting it again"),
            Err(e) => {
                tracing::warn!("ruuvi: {e}; retrying in {backoff} s");
                let _ = scan.send((Scan::Error(e.to_string()), now_s()));
            }
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

async fn session(tx: &mpsc::Sender<Advert>, scan: &watch::Sender<(Scan, f64)>) -> bluer::Result<()> {
    let session = Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await.ok(); // already on for the batteries; never an error here
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: true,
            ..Default::default()
        })
        .await?;
    // with changes: a device is reported again when its properties change, so one whose manufacturer data wasn't
    // known yet when it first appeared is checked again on its next advertisement
    let events = adapter.discover_devices_with_changes().await?; // the session runs while this is held
    let _ = scan.send((Scan::Scanning, now_s()));
    tracing::info!("ruuvi: listening on {}", adapter.name());
    let watched: Watched = Arc::new(Mutex::new(HashSet::new()));
    let mut skipped: HashMap<Address, Instant> = HashMap::new();
    let mut events = std::pin::pin!(events);
    while let Some(ev) = events.next().await {
        let AdapterEvent::DeviceAdded(addr) = ev else { continue };
        if watched.lock().unwrap().contains(&addr) {
            continue;
        }
        if skipped.get(&addr).is_some_and(|until| Instant::now() < *until) {
            continue;
        }
        let Ok(dev) = adapter.device(addr) else { continue };
        let until = match dev.manufacturer_data().await {
            Ok(Some(m)) if m.contains_key(&proto_ruuvi::COMPANY_ID) => None,
            Ok(Some(_)) => Some(RECHECK),     // someone else's device
            _ => Some(UNKNOWN_RECHECK),       // no manufacturer data known yet
        };
        if let Some(d) = until {
            skipped.insert(addr, Instant::now() + d);
            if skipped.len() > 2000 {
                skipped.retain(|_, until| Instant::now() < *until);
            }
            continue;
        }
        skipped.remove(&addr);
        let Ok(dev_events) = dev.events().await else { continue };
        // RSSI is signalled only when it changes enough: start from the current value
        let rssi0 = dev.rssi().await.ok().flatten();
        watched.lock().unwrap().insert(addr);
        let (tx, watched) = (tx.clone(), watched.clone());
        let a = norm_addr(&addr.to_string()).unwrap_or_else(|| addr.to_string());
        tokio::spawn(async move {
            let mut dev_events = std::pin::pin!(dev_events);
            let mut rssi = rssi0;
            while let Some(DeviceEvent::PropertyChanged(p)) = dev_events.next().await {
                match p {
                    DeviceProperty::Rssi(r) => rssi = Some(r),
                    DeviceProperty::ManufacturerData(m) => {
                        if let Some(data) = m.get(&proto_ruuvi::COMPANY_ID)
                            && tx.send(Advert { addr: a.clone(), data: data.clone(), rssi }).await.is_err()
                        {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            watched.lock().unwrap().remove(&addr); // removed by BlueZ (unheard 30 s) or the session ended
        });
    }
    Ok(())
}
