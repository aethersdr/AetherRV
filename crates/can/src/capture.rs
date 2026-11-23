//! Capture files in python-can's `candump -l` format, so `canplayer`, SavvyCAN, python-can and the Python reference
//! read them unchanged:
//!
//! ```text
//! (1700000000.123456) hat0 09F20123#0101010101010101 R
//! (1700000000.223456) hat1 054#0905D2007E03 T
//! (1700000000.323456) hat0 20000080#
//! ```
//!
//! One file per bus open, `<bus>-<YYYYmmdd-HHMMSS>.log`, rotated at 50 MB to
//! `<bus>-<stamp>_<YYYY-mm-ddTHHMMSS>_#NNN.log` (python-can `SizedRotatingLogger` naming).

use crate::frame::{CanFrame, hex_upper};
use std::fs::File;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 50_000_000;
const CAN_ERR_FLAG: u32 = 0x2000_0000;
const CAN_ERR_BUSERROR: u32 = 0x0000_0080;

/// One capture line (python-can `CanutilsLogWriter`). `last_ts` keeps timestamps monotonic, as python-can does.
pub fn format_line(f: &CanFrame, channel: &str, last_ts: &mut Option<f64>) -> String {
    let lt = last_ts.get_or_insert(f.ts);
    let ts = if f.ts < *lt { *lt } else { f.ts };
    *lt = ts;
    let mut s = format!("({ts:.6}) {channel}");
    if f.error {
        s += &format!(" {:08X}#", CAN_ERR_FLAG | CAN_ERR_BUSERROR);
        s.push('\n');
        return s;
    }
    if f.extended {
        s += &format!(" {:08X}#", f.id);
    } else {
        s += &format!(" {:03X}#", f.id);
    }
    let eol = if f.is_rx { " R\n" } else { " T\n" };
    if f.remote {
        s.push('R');
    } else {
        s += &hex_upper(&f.data);
    }
    s += eol;
    s
}

/// Parse one capture line (also plain `candump -l` lines without the R/T suffix).
pub fn parse_line(line: &str) -> Option<(f64, String, CanFrame)> {
    let line = line.trim();
    let (ts, rest) = line.strip_prefix('(')?.split_once(')')?;
    let ts: f64 = ts.parse().ok()?;
    let mut parts = rest.split_whitespace();
    let channel = parts.next()?.to_string();
    let frame = parts.next()?;
    let suffix = parts.next();
    let (id_s, data_s) = frame.split_once('#')?;
    let id = u32::from_str_radix(id_s, 16).ok()?;
    let is_rx = suffix != Some("T");
    if id & CAN_ERR_FLAG != 0 && id & CAN_ERR_BUSERROR != 0 && id_s.len() == 8 {
        return Some((ts, channel, CanFrame { id: id & 0x1FFF_FFFF, data: vec![], extended: true, remote: false, error: true, is_rx, ts }));
    }
    let extended = id_s.len() > 3;
    let remote = data_s.starts_with('R');
    let data = if remote {
        vec![]
    } else {
        (0..data_s.len() / 2).filter_map(|i| u8::from_str_radix(&data_s[2 * i..2 * i + 2], 16).ok()).collect()
    };
    Some((ts, channel, CanFrame { id, data, extended, remote, error: false, is_rx, ts }))
}

/// Read a whole capture.
pub fn read_file(path: &Path) -> io::Result<Vec<CanFrame>> {
    let f = File::open(path)?;
    Ok(io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| parse_line(&l).map(|x| x.2)).collect())
}

pub struct CaptureWriter {
    dir: PathBuf,
    channel: String,
    base: PathBuf,
    file: BufWriter<File>,
    written: u64,
    rollovers: u32,
    last_ts: Option<f64>,
    max_bytes: u64,
}

impl CaptureWriter {
    /// Start `<dir>/<bus>-<local stamp>.log`.
    pub fn open(dir: &Path, bus: &str) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let base = dir.join(format!("{bus}-{}.log", local_stamp("%Y%m%d-%H%M%S")));
        let file = BufWriter::with_capacity(64 * 1024, File::options().create(true).append(true).open(&base)?);
        tracing::info!("[{bus}] capturing to {}", base.display());
        Ok(CaptureWriter {
            dir: dir.to_path_buf(),
            channel: bus.to_string(),
            base,
            file,
            written: 0,
            rollovers: 0,
            last_ts: None,
            max_bytes: MAX_BYTES,
        })
    }

    pub fn path(&self) -> &Path {
        &self.base
    }

    pub fn set_max_bytes(&mut self, n: u64) {
        self.max_bytes = n;
    }

    pub fn write(&mut self, f: &CanFrame) -> io::Result<()> {
        let line = format_line(f, &self.channel, &mut self.last_ts);
        if self.max_bytes > 0 && self.written + line.len() as u64 > self.max_bytes && self.written > 0 {
            self.rollover()?;
        }
        self.file.write_all(line.as_bytes())?;
        self.written += line.len() as u64;
        Ok(())
    }

    fn rollover(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let stem = self.base.file_stem().and_then(|s| s.to_str()).unwrap_or("capture").split('.').next().unwrap_or("capture").to_string();
        let rotated = self.dir.join(format!("{stem}_{}_#{:03}.log", local_stamp("%Y-%m-%dT%H%M%S"), self.rollovers));
        // (the count is used before it's incremented: #000, #001, …)
        self.rollovers += 1; // python-can numbers the first rotated file #000
        std::fs::rename(&self.base, &rotated)?;
        self.file = BufWriter::with_capacity(64 * 1024, File::options().create(true).append(true).open(&self.base)?);
        self.written = 0;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for CaptureWriter {
    fn drop(&mut self) {
        let _ = self.file.flush();
    }
}

/// `strftime` in local time.
pub fn local_stamp(fmt: &str) -> String {
    let t = aetherrv_core::clock::now_s() as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let cfmt = std::ffi::CString::new(fmt).unwrap();
    let mut buf = [0u8; 64];
    // SAFETY: localtime_r/strftime write into buffers we own.
    let n = unsafe {
        libc::localtime_r(&t, &mut tm);
        libc::strftime(buf.as_mut_ptr() as *mut _, buf.len(), cfmt.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..n]).to_string()
}

/// Delete capture logs older than `keep_days`, then oldest-first until under `max_gb` (Python `prune_captures`,
/// one pass). Returns the deleted paths.
pub fn prune(dir: &Path, keep_days: f64, max_gb: f64, now: f64) -> Vec<PathBuf> {
    let mut files: Vec<(f64, u64, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".log"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                if !m.is_file() {
                    return None;
                }
                let mt = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
                Some((mt, m.len(), e.path()))
            })
            .collect(),
        Err(_) => return vec![],
    };
    files.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let cutoff = now - keep_days * 86400.0;
    let mut gone = vec![];
    files.retain(|(mt, _, p)| {
        if *mt < cutoff {
            let _ = std::fs::remove_file(p);
            gone.push(p.clone());
            false
        } else {
            true
        }
    });
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut i = 0;
    while i < files.len() && total as f64 > max_gb * 1e9 {
        let _ = std::fs::remove_file(&files[i].2);
        total -= files[i].1;
        gone.push(files[i].2.clone());
        i += 1;
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format_matches_python_can() {
        let mut lt = None;
        let f = CanFrame::ext(0x09F20123, &[1; 8]).with_ts(1700000000.5);
        assert_eq!(format_line(&f, "t", &mut lt), "(1700000000.500000) t 09F20123#0101010101010101 R\n");
        let s = CanFrame::std(0x54, &[0x09, 0x05]).with_ts(1700000000.4).tx();
        assert_eq!(format_line(&s, "hat1", &mut lt), "(1700000000.500000) hat1 054#0905 T\n", "monotonic timestamps");
        let (ts, ch, back) = parse_line("(1700000000.500000) hat1 054#0905 T").unwrap();
        assert_eq!((ts, ch.as_str(), back.id, back.extended, back.is_rx, back.data), (1700000000.5, "hat1", 0x54, false, false, vec![9, 5]));
        let (_, _, plain) = parse_line("(1.000000) can0 18EEFF00#0102").unwrap();
        assert!(plain.extended && plain.is_rx);
    }

    #[test]
    fn rotation_names_like_python_can() {
        let d = tempfile::tempdir().unwrap();
        let mut w = CaptureWriter::open(d.path(), "hat0").unwrap();
        w.set_max_bytes(200);
        for i in 0..20 {
            w.write(&CanFrame::ext(0x09F20100 + i, &[i as u8; 8]).with_ts(1.0 + i as f64)).unwrap();
        }
        w.flush().unwrap();
        let names: Vec<String> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        assert!(names.iter().any(|n| n.contains("_#000.log")), "{names:?}");
        let total: usize = std::fs::read_dir(d.path()).unwrap().map(|e| read_file(&e.unwrap().path()).unwrap().len()).sum();
        assert_eq!(total, 20);
    }
}
