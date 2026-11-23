//! socketcand client (CAN over TCP, e.g. the Cerbo GX's VE.Can): the python-can protocol.
//!
//! ```text
//! server: < hi >          client: < open can0 >   server: < ok >   client: < rawmode >   server: < ok >
//! frames: < frame 09F20123 1700000000.123456 0102030405060708 >   (3 hex digits = standard id, else extended)
//! errors: < error 20000080 1700000000.123456 >
//! send:   < send 123 2 1 2 >                                      (id, dlc in hex, data bytes in hex)
//! ```

use crate::frame::CanFrame;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

/// The reading half of a socketcand connection (the writing half is returned by [`Socketcand::connect`]).
pub struct Socketcand {
    rd: OwnedReadHalf,
    buf: String,
}

impl Socketcand {
    /// Connect and enter raw mode (10 s connect budget, as python-can).
    pub async fn connect(host: &str, port: u16, channel: &str) -> io::Result<(Self, OwnedWriteHalf)> {
        let s = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host, port)))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "socketcand connect timed out"))??;
        let _ = s.set_nodelay(true);
        let (rd, mut wr) = s.into_split();
        let mut sc = Socketcand { rd, buf: String::new() };
        sc.expect("< hi >").await?;
        wr.write_all(format!("< open {channel} >").as_bytes()).await?;
        sc.expect("< ok >").await?;
        wr.write_all(b"< rawmode >").await?;
        sc.expect("< ok >").await?;
        Ok((sc, wr))
    }

    async fn expect(&mut self, want: &str) -> io::Result<()> {
        let got = tokio::time::timeout(Duration::from_secs(5), self.next_msg())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("socketcand: no {want}")))??;
        if got != want {
            return Err(io::Error::other(format!("socketcand: expected {want}, got {got}")));
        }
        Ok(())
    }

    /// The next complete `< ... >` message.
    pub async fn next_msg(&mut self) -> io::Result<String> {
        loop {
            if let Some(start) = self.buf.find('<')
                && let Some(end) = self.buf[start..].find('>')
            {
                let m = self.buf[start..start + end + 1].to_string();
                self.buf.drain(..start + end + 1);
                return Ok(m);
            }
            let mut b = [0u8; 4096];
            let n = self.rd.read(&mut b).await?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "socketcand closed the connection"));
            }
            self.buf.push_str(&String::from_utf8_lossy(&b[..n]));
        }
    }
}

/// Parse a received message into a frame (`None` for anything that isn't a frame or error).
pub fn parse(msg: &str) -> Option<CanFrame> {
    let body = msg.strip_suffix(" >")?;
    let (kind, rest) = if let Some(r) = body.strip_prefix("< frame ") {
        ("frame", r)
    } else if let Some(r) = body.strip_prefix("< error ") {
        ("error", r)
    } else {
        return None;
    };
    let mut parts = rest.splitn(3, ' ');
    let id_s = parts.next()?;
    let id = u32::from_str_radix(id_s, 16).ok()?;
    let ts: f64 = parts.next()?.parse().ok()?;
    if kind == "error" {
        return Some(CanFrame { id: id & 0x1FFF_FFFF, data: vec![0], extended: true, remote: false, error: true, is_rx: true, ts });
    }
    let hex = parts.next().unwrap_or("").trim();
    let data: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    Some(CanFrame { id, data, extended: id_s.len() != 3, remote: false, error: false, is_rx: true, ts })
}

/// The `< send ... >` line for a frame.
pub fn send_line(f: &CanFrame) -> String {
    let id = if f.extended { format!("{:08X}", f.id & 0x1FFF_FFFF) } else { format!("{:03X}", f.id & 0x7FF) };
    let bytes: Vec<String> = f.data.iter().map(|b| format!("{b:x}")).collect();
    format!("< send {id} {:X} {} >", f.data.len(), bytes.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_like_python_can() {
        let f = parse("< frame 09F20123 1700000000.250000 0102 >").unwrap();
        assert_eq!((f.id, f.extended, f.data.clone(), f.ts), (0x09F20123, true, vec![1, 2], 1700000000.25));
        let s = parse("< frame 2C4 1.5 0903 >").unwrap();
        assert!(!s.extended && s.id == 0x2C4);
        let e = parse("< error 20000080 2.0 >").unwrap();
        assert!(e.error);
        assert!(parse("< ok >").is_none());
        assert_eq!(send_line(&CanFrame::std(0x123, &[1, 0xAB])), "< send 123 2 1 ab >");
        assert_eq!(send_line(&CanFrame::ext(0x0DED0096, &[])), "< send 0DED0096 0  >");
    }
}
