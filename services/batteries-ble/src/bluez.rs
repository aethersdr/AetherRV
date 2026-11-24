//! The real BlueZ layer (bluer over D-Bus), replacing bleak + `bluetoothctl`.
//!
//! * `find`: a discovery session until the battery is seen advertising (its `RSSI` is set, which BlueZ does only
//!   for devices heard during discovery), like `BleakScanner.find_device_by_address`.
//! * `connect`: `Device.Connect`, then the GATT services (bluer waits for `ServicesResolved`) and the UART
//!   characteristic `ffe1`, all within the connect timeout, like `BleakClient.connect()`.
//! * notifications use `StartNotify` (`Characteristic::notify`, not `AcquireNotify`), as Python's
//!   `use_start_notify=True`; writes are "command" (write without response).
//!
//! Not exercised by the tests (no Bluetooth here); the logic above it is, through the mock.

use crate::{Ble, BleError, Link};
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::gatt::WriteOp;
use bluer::{Adapter, Address, AdapterEvent, Device, DeviceEvent, DeviceProperty, Session, Uuid};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

fn err(e: bluer::Error) -> BleError {
    BleError::bluez(e.to_string())
}

fn addr(a: &str) -> Result<Address, BleError> {
    a.parse().map_err(|_| BleError::connection(format!("bad address {a}")))
}

/// The D-Bus session is opened on first use and again after a failure, so bluetoothd being down (or restarting)
/// shows as each battery's error and is retried with its backoff, as bleak does; the process never exits for it.
pub struct BlueZ {
    adapter: tokio::sync::Mutex<Option<(Session, Adapter)>>,
    uart: Uuid,
}

impl Default for BlueZ {
    fn default() -> Self {
        Self::new()
    }
}

impl BlueZ {
    pub fn new() -> BlueZ {
        BlueZ { adapter: tokio::sync::Mutex::new(None), uart: Uuid::parse_str(proto_roypow::UART).expect("the UART uuid") }
    }

    async fn adapter(&self) -> Result<Adapter, BleError> {
        let mut g = self.adapter.lock().await;
        if let Some((_, a)) = g.as_ref() {
            return Ok(a.clone());
        }
        let session = Session::new().await.map_err(err)?;
        let a = session.default_adapter().await.map_err(err)?;
        *g = Some((session, a.clone()));
        Ok(a)
    }

    /// Forget the session after a D-Bus failure (the next call opens a new one).
    async fn reset(&self) {
        *self.adapter.lock().await = None;
    }

    async fn advertising(&self, adapter: &Adapter, a: Address) -> bool {
        match adapter.device(a) {
            Ok(d) => matches!(d.rssi().await, Ok(Some(_))),
            Err(_) => false,
        }
    }

    async fn uart_of(&self, dev: &Device) -> Result<Characteristic, BleError> {
        for s in dev.services().await.map_err(err)? {
            for c in s.characteristics().await.map_err(err)? {
                if c.uuid().await.map_err(err)? == self.uart {
                    return Ok(c);
                }
            }
        }
        Err(BleError::connection("no UART characteristic ffe1"))
    }
}

impl Ble for BlueZ {
    type Link = BlueZLink;

    async fn find(&self, a: &str, timeout: Duration) -> Result<bool, BleError> {
        let a = addr(a)?;
        let adapter = self.adapter().await?;
        let events = match adapter.discover_devices().await {
            Ok(e) => e, // discovery runs while this is held
            Err(e) => {
                self.reset().await;
                return Err(err(e));
            }
        };
        let mut events = std::pin::pin!(events);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(AdapterEvent::DeviceAdded(x)) if x == a => {
                        if self.advertising(&adapter, a).await {
                            return Ok(true);
                        }
                    }
                    Some(_) => {}
                    None => return Ok(false),
                },
                _ = tick.tick() => {
                    if self.advertising(&adapter, a).await {
                        return Ok(true);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => return Ok(false),
            }
        }
    }

    async fn connect(&self, a: &str, timeout: Duration) -> Result<BlueZLink, BleError> {
        let dev = self.adapter().await?.device(addr(a)?).map_err(err)?;
        let events = dev.events().await.map_err(err)?;
        let (lost_tx, lost_rx) = watch::channel(false);
        let watcher = tokio::spawn(async move {
            let mut events = std::pin::pin!(events);
            while let Some(ev) = events.next().await {
                if let DeviceEvent::PropertyChanged(DeviceProperty::Connected(false)) = ev {
                    let _ = lost_tx.send(true);
                }
            }
            let _ = lost_tx.send(true); // the device object went away
        });
        let r = tokio::time::timeout(timeout, async {
            dev.connect().await.map_err(err)?;
            self.uart_of(&dev).await
        })
        .await;
        match r {
            Ok(Ok(uart)) => Ok(BlueZLink { dev, uart, lost: lost_rx, tasks: vec![watcher] }),
            Ok(Err(e)) => {
                watcher.abort();
                Err(e)
            }
            Err(_) => {
                watcher.abort();
                Err(BleError::timeout("")) // asyncio.TimeoutError from bleak's connect timeout
            }
        }
    }

    async fn disconnect(&self, a: &str) {
        if let Ok(a) = addr(a)
            && let Ok(adapter) = self.adapter().await
            && let Ok(d) = adapter.device(a)
        {
            let _ = tokio::time::timeout(crate::CLEAR, d.disconnect()).await;
        }
    }
}

pub struct BlueZLink {
    dev: Device,
    uart: Characteristic,
    lost: watch::Receiver<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for BlueZLink {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Link for BlueZLink {
    async fn start_notify(&mut self) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, BleError> {
        let stream = self.uart.notify().await.map_err(err)?; // StartNotify; the stream holds the session
        let (tx, rx) = mpsc::unbounded_channel();
        self.tasks.push(tokio::spawn(async move {
            let mut stream = std::pin::pin!(stream);
            while let Some(v) = stream.next().await {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }));
        Ok(rx)
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), BleError> {
        let req = CharacteristicWriteRequest { op_type: WriteOp::Command, ..Default::default() };
        self.uart.write_ext(data, &req).await.map_err(err)
    }

    fn lost(&self) -> watch::Receiver<bool> {
        self.lost.clone()
    }

    async fn disconnect(&mut self) {
        for t in self.tasks.drain(1..) {
            t.abort(); // the notify session (StopNotify)
        }
        let _ = tokio::time::timeout(crate::CLEAR, self.dev.disconnect()).await;
    }
}
