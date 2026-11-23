//! A classic CAN frame as AetherRV sees it (python-can's `Message` fields that matter here).

#[derive(Debug, Clone, PartialEq)]
pub struct CanFrame {
    /// 11- or 29-bit identifier (no flag bits).
    pub id: u32,
    pub data: Vec<u8>,
    pub extended: bool,
    pub remote: bool,
    pub error: bool,
    /// False for our own frame coming back once really sent (SocketCAN `MSG_CONFIRM`), true for everything else.
    pub is_rx: bool,
    /// Receive time, Unix seconds.
    pub ts: f64,
}

impl CanFrame {
    pub fn std(id: u32, data: &[u8]) -> Self {
        CanFrame { id: id & 0x7FF, data: data.to_vec(), extended: false, remote: false, error: false, is_rx: true, ts: 0.0 }
    }

    pub fn ext(id: u32, data: &[u8]) -> Self {
        CanFrame { id: id & 0x1FFF_FFFF, data: data.to_vec(), extended: true, remote: false, error: false, is_rx: true, ts: 0.0 }
    }

    pub fn with_ts(mut self, ts: f64) -> Self {
        self.ts = ts;
        self
    }

    pub fn tx(mut self) -> Self {
        self.is_rx = false;
        self
    }

    /// `123#0102` / `09F20123#...` as candump prints it.
    pub fn short(&self) -> String {
        let id = if self.extended { format!("{:08X}", self.id) } else { format!("{:03X}", self.id) };
        format!("{id}#{}", hex_upper(&self.data))
    }
}

pub fn hex_upper(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02X}")).collect()
}

/// `11 22 33` (Python `bytes.hex(" ")`).
pub fn hex_spaced(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}
