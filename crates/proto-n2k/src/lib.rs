//! NMEA 2000 protocol for AetherRV (port of `adapters/n2k.py`, `adapters/n2k_pgns.py` and the encoding half of
//! `switching.py`). Pure: no I/O, no clock reads; time is passed in as `now` (Unix seconds, like `time.time()`).
//!
//! * [`id`]          29-bit identifier split (J1939 PDU1 / PDU2)
//! * [`fast_packet`] fast-packet reassembly
//! * [`decode`]      PGN decoders (127501, 126208 header, 60928, 126996, 127508, 127507, 127500, 127751)
//! * [`encode`]      126208 command encoding, fast-packet framing, CAN ids, the ISO NAME (what AetherRV sends)
//! * [`adapter`]     the read-only adapter: bank tracking, device inventory, alternator, the published state keys
//! * [`energy`]      `energy.derive`: alternator power and DC load from the Cerbo's and the regulator's readings
//! * [`pgns`]        the CANboat-derived PGN table (Apache-2.0 data)

pub mod adapter;
pub mod decode;
pub mod encode;
pub mod energy;
pub mod fast_packet;
pub mod id;
pub mod pgns;
mod pyfmt;

#[cfg(test)]
mod testutil;

pub use adapter::{Alternator, Bank, BankListener, Command, Device, LabelError, Load, N2kAdapter, Volts};
pub use decode::{BankStates, DecodeError, SwitchState};
pub use encode::EncodeError;
pub use fast_packet::FastPacketAssembler;
pub use id::{N2kId, parse_id};

/// One received CAN frame, as the CAN crate hands it over (python-can `Message` fields the adapter reads).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// Arbitration id (29-bit when `extended`).
    pub id: u32,
    pub data: Vec<u8>,
    pub extended: bool,
    pub error: bool,
    pub remote: bool,
}

impl Frame {
    /// An extended data frame (what every NMEA 2000 frame is).
    pub fn extended(id: u32, data: impl Into<Vec<u8>>) -> Self {
        Self { id, data: data.into(), extended: true, error: false, remote: false }
    }
}
