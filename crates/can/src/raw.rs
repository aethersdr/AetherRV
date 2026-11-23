//! Raw SocketCAN (`PF_CAN`/`SOCK_RAW`/`CAN_RAW`), non-blocking, on tokio.
//!
//! * Error frames are received (`CAN_RAW_ERR_FILTER = CAN_ERR_MASK`, as python-can does).
//! * `own_echo`: `CAN_RAW_RECV_OWN_MSGS` on; our own frames come back flagged `MSG_CONFIRM` once the controller
//!   really sent them (the TX-echo guard of [`crate::FailoverBus`]). Without it this socket never hears itself.
//! * Frames sent by *other* sockets on the same interface are always heard (`CAN_RAW_LOOPBACK` default on), which
//!   is why one process owns each port (ARCHITECTURE.md §2).
//! * A full TX queue is an error (`ENOBUFS`), never a block.

use crate::frame::CanFrame;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use tokio::io::unix::AsyncFd;

const CAN_EFF_FLAG: u32 = 0x8000_0000;
const CAN_RTR_FLAG: u32 = 0x4000_0000;
const CAN_ERR_FLAG: u32 = 0x2000_0000;
const CAN_ERR_MASK: u32 = 0x1FFF_FFFF;
const SOL_CAN_RAW: libc::c_int = 101;
const CAN_RAW_ERR_FILTER: libc::c_int = 2;
const CAN_RAW_RECV_OWN_MSGS: libc::c_int = 4;
const MSG_CONFIRM: libc::c_int = 0x800;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RawFrame {
    can_id: u32,
    len: u8,
    _pad: u8,
    _res0: u8,
    _len8_dlc: u8,
    data: [u8; 8],
}

#[repr(C)]
struct SockaddrCan {
    can_family: libc::sa_family_t,
    can_ifindex: libc::c_int,
    _addr: [u8; 16],
}

pub struct RawSocket {
    fd: AsyncFd<OwnedFd>,
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

impl RawSocket {
    pub fn open(iface: &str, own_echo: bool) -> io::Result<Self> {
        let name = std::ffi::CString::new(iface).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad name"))?;
        // SAFETY: plain libc socket calls on a fd we own.
        unsafe {
            let idx = libc::if_nametoindex(name.as_ptr());
            if idx == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, format!("no such CAN interface {iface}")));
            }
            let fd = cvt(libc::socket(libc::PF_CAN, libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, libc::CAN_RAW))?;
            let owned = OwnedFd::from_raw_fd(fd);
            let mask: u32 = CAN_ERR_MASK;
            cvt(libc::setsockopt(fd, SOL_CAN_RAW, CAN_RAW_ERR_FILTER, &mask as *const u32 as *const _, 4))?;
            let own: libc::c_int = own_echo as libc::c_int;
            cvt(libc::setsockopt(fd, SOL_CAN_RAW, CAN_RAW_RECV_OWN_MSGS, &own as *const _ as *const _, 4))?;
            let addr = SockaddrCan { can_family: libc::AF_CAN as _, can_ifindex: idx as _, _addr: [0; 16] };
            cvt(libc::bind(fd, &addr as *const _ as *const libc::sockaddr, std::mem::size_of::<SockaddrCan>() as _))?;
            // SAFETY: we own the fd and never close it while it's registered (OwnedFd drops with the AsyncFd).
            let fd = AsyncFd::register(owned).map_err(|e| io::Error::other(e.to_string()))?;
            Ok(RawSocket { fd })
        }
    }

    fn raw(&self) -> RawFd {
        self.fd.get_ref().as_raw_fd()
    }

    pub async fn recv(&self) -> io::Result<CanFrame> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| recv_once(inner.get_ref().as_raw_fd())) {
                Ok(r) => return r,
                Err(_would_block) => continue,
            }
        }
    }

    /// Non-blocking send.
    pub fn send(&self, f: &CanFrame) -> io::Result<()> {
        let mut rf = RawFrame { len: f.data.len().min(8) as u8, ..Default::default() };
        rf.can_id = if f.extended { (f.id & CAN_ERR_MASK) | CAN_EFF_FLAG } else { f.id & 0x7FF };
        if f.remote {
            rf.can_id |= CAN_RTR_FLAG;
        }
        rf.data[..rf.len as usize].copy_from_slice(&f.data[..rf.len as usize]);
        // SAFETY: writing a fully initialised can_frame to our socket.
        let n = unsafe { libc::write(self.raw(), &rf as *const _ as *const _, std::mem::size_of::<RawFrame>()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn recv_once(fd: RawFd) -> io::Result<CanFrame> {
    let mut rf = RawFrame::default();
    let mut iov = libc::iovec { iov_base: &mut rf as *mut _ as *mut _, iov_len: std::mem::size_of::<RawFrame>() };
    let mut addr: SockaddrCan = SockaddrCan { can_family: 0, can_ifindex: 0, _addr: [0; 16] };
    // SAFETY: msghdr points at buffers we own for the duration of the call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_name = &mut addr as *mut _ as *mut _;
    msg.msg_namelen = std::mem::size_of::<SockaddrCan>() as _;
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    if (n as usize) < 8 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short CAN frame"));
    }
    let id = rf.can_id;
    let len = (rf.len as usize).min(8);
    Ok(CanFrame {
        id: if id & CAN_EFF_FLAG != 0 || id & CAN_ERR_FLAG != 0 { id & CAN_ERR_MASK } else { id & 0x7FF },
        extended: id & CAN_EFF_FLAG != 0 || id & CAN_ERR_FLAG != 0,
        remote: id & CAN_RTR_FLAG != 0,
        error: id & CAN_ERR_FLAG != 0,
        data: rf.data[..len].to_vec(),
        is_rx: msg.msg_flags & MSG_CONFIRM == 0,
        ts: aetherrv_core::clock::now_s(),
    })
}

/// Kernel interface admin state (`IFF_UP` in `/sys/class/net/<if>/flags`). A bound socket survives
/// `ip link set down` silently, so this is the honest "is the bus usable" signal.
pub fn iface_up(iface: &str) -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{iface}/flags"))
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .is_some_and(|f| f & 0x1 != 0)
}
