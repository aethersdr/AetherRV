//! A virtual multitouch touchscreen on /dev/uinput (absolute MT protocol B, one slot, BTN_TOUCH, INPUT_PROP_DIRECT),
//! so libinput and sway treat it exactly like the van's capacitive panel. Needs root (/dev/uinput is 0600).

use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const INPUT_PROP_DIRECT: i32 = 0x01;
const BUS_VIRTUAL: u16 = 0x06;

// _IOW('U', nr, size) = 1<<30 | size<<16 | 'U'<<8 | nr (generic ioctl encoding: arm64, x86_64)
const fn iow(nr: u64, size: u64) -> u64 {
    (1 << 30) | (size << 16) | (0x55 << 8) | nr
}
const UI_SET_EVBIT: u64 = iow(100, 4);
const UI_SET_KEYBIT: u64 = iow(101, 4);
const UI_SET_ABSBIT: u64 = iow(103, 4);
const UI_SET_PROPBIT: u64 = iow(110, 4);
const UI_DEV_SETUP: u64 = iow(3, 92);
const UI_ABS_SETUP: u64 = iow(4, 28);
const UI_DEV_CREATE: u64 = 0x5501;
const UI_DEV_DESTROY: u64 = 0x5502;
const UI_GET_SYSNAME_64: u64 = (2 << 30) | (64 << 16) | (0x55 << 8) | 44;

pub const VENDOR: u16 = 0x1d6b; // "Linux Foundation" virtual ids, as used by many uinput tools
pub const PRODUCT: u16 = 0x0bec;
pub const NAME: &str = "uibench touchscreen";

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct AbsSetup {
    code: u16,
    _pad: u16,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

pub struct Touch {
    f: File,
    tracking: i32,
    pub sysname: String,
}

fn ioctl_int(fd: i32, req: u64, v: i32) -> Result<()> {
    let r = unsafe { libc::ioctl(fd, req as _, v as libc::c_int) };
    if r < 0 {
        bail!("ioctl {req:#x}: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

impl Touch {
    /// Create the device with an absolute range of 0..w-1 × 0..h-1 (output pixels; sway maps it to its output).
    pub fn create(w: i32, h: i32) -> Result<Self> {
        let f = OpenOptions::new().write(true).open("/dev/uinput").context("open /dev/uinput (needs root)")?;
        let fd = f.as_raw_fd();
        ioctl_int(fd, UI_SET_EVBIT, EV_KEY as i32)?;
        ioctl_int(fd, UI_SET_EVBIT, EV_ABS as i32)?;
        ioctl_int(fd, UI_SET_EVBIT, EV_SYN as i32)?;
        ioctl_int(fd, UI_SET_KEYBIT, BTN_TOUCH as i32)?;
        ioctl_int(fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        // resolution in units/mm: the van panel is 10.1" 1920x1200, about 8.9 px/mm
        let abs = |code: u16, max: i32, res: i32| -> Result<()> {
            ioctl_int(fd, UI_SET_ABSBIT, code as i32)?;
            let s = AbsSetup { code, _pad: 0, value: 0, minimum: 0, maximum: max, fuzz: 0, flat: 0, resolution: res };
            if unsafe { libc::ioctl(fd, UI_ABS_SETUP as _, &s as *const AbsSetup) } < 0 {
                bail!("UI_ABS_SETUP {code:#x}: {}", std::io::Error::last_os_error());
            }
            Ok(())
        };
        abs(ABS_X, w - 1, 9)?;
        abs(ABS_Y, h - 1, 9)?;
        abs(ABS_MT_SLOT, 9, 0)?;
        abs(ABS_MT_POSITION_X, w - 1, 9)?;
        abs(ABS_MT_POSITION_Y, h - 1, 9)?;
        abs(ABS_MT_TRACKING_ID, 65535, 0)?;
        let mut name = [0u8; 80];
        name[..NAME.len()].copy_from_slice(NAME.as_bytes());
        let setup = UinputSetup { bustype: BUS_VIRTUAL, vendor: VENDOR, product: PRODUCT, version: 1, name, ff_effects_max: 0 };
        if unsafe { libc::ioctl(fd, UI_DEV_SETUP as _, &setup as *const UinputSetup) } < 0 {
            bail!("UI_DEV_SETUP: {}", std::io::Error::last_os_error());
        }
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE as _) } < 0 {
            bail!("UI_DEV_CREATE: {}", std::io::Error::last_os_error());
        }
        let mut buf = [0u8; 64];
        let sysname = if unsafe { libc::ioctl(fd, UI_GET_SYSNAME_64 as _, buf.as_mut_ptr()) } >= 0 {
            String::from_utf8_lossy(&buf[..buf.iter().position(|&b| b == 0).unwrap_or(0)]).to_string()
        } else {
            String::new()
        };
        Ok(Touch { f, tracking: 0, sysname })
    }

    /// The /dev/input/eventN node of the device (after udev has created it).
    pub fn event_node(&self) -> Option<String> {
        let dir = format!("/sys/devices/virtual/input/{}", self.sysname);
        std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("event").then(|| format!("/dev/input/{n}"))
        })
    }

    fn emit(&mut self, evs: &[(u16, u16, i32)]) -> Result<(u64, u64)> {
        let mut b = Vec::with_capacity(evs.len() * 24);
        for &(t, c, v) in evs {
            b.extend_from_slice(&[0u8; 16]); // struct timeval: the kernel stamps uinput events itself
            b.extend_from_slice(&t.to_ne_bytes());
            b.extend_from_slice(&c.to_ne_bytes());
            b.extend_from_slice(&v.to_ne_bytes());
        }
        let t0 = crate::mono_ns();
        self.f.write_all(&b)?;
        let t1 = crate::mono_ns();
        Ok((t0, t1))
    }

    pub fn down(&mut self, x: i32, y: i32) -> Result<(u64, u64)> {
        self.tracking = (self.tracking + 1) & 0xffff;
        self.emit(&[
            (EV_ABS, ABS_MT_SLOT, 0),
            (EV_ABS, ABS_MT_TRACKING_ID, self.tracking),
            (EV_ABS, ABS_MT_POSITION_X, x),
            (EV_ABS, ABS_MT_POSITION_Y, y),
            (EV_KEY, BTN_TOUCH, 1),
            (EV_ABS, ABS_X, x),
            (EV_ABS, ABS_Y, y),
            (EV_SYN, SYN_REPORT, 0),
        ])
    }

    pub fn moveto(&mut self, x: i32, y: i32) -> Result<(u64, u64)> {
        self.emit(&[
            (EV_ABS, ABS_MT_SLOT, 0),
            (EV_ABS, ABS_MT_POSITION_X, x),
            (EV_ABS, ABS_MT_POSITION_Y, y),
            (EV_ABS, ABS_X, x),
            (EV_ABS, ABS_Y, y),
            (EV_SYN, SYN_REPORT, 0),
        ])
    }

    pub fn up(&mut self) -> Result<(u64, u64)> {
        self.emit(&[(EV_ABS, ABS_MT_SLOT, 0), (EV_ABS, ABS_MT_TRACKING_ID, -1), (EV_KEY, BTN_TOUCH, 0), (EV_SYN, SYN_REPORT, 0)])
    }
}

impl Drop for Touch {
    fn drop(&mut self) {
        unsafe { libc::ioctl(self.f.as_raw_fd(), UI_DEV_DESTROY as _) };
    }
}
