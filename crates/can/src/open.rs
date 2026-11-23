//! The real transports.

use crate::bus::{BoxFut, Link, Opener, Sender};
use crate::frame::CanFrame;
use crate::raw::RawSocket;
use crate::socketcand::{Socketcand, parse, send_line};
use std::io;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub struct SocketCanOpener {
    pub iface: String,
    pub own_echo: bool,
}

struct RawSender(Arc<RawSocket>);

impl Sender for RawSender {
    fn send(&self, f: &CanFrame) -> io::Result<()> {
        self.0.send(f)
    }
}

impl Opener for SocketCanOpener {
    fn open(&self) -> BoxFut<io::Result<Link>> {
        let (iface, own) = (self.iface.clone(), self.own_echo);
        Box::pin(async move {
            let sock = Arc::new(RawSocket::open(&iface, own)?);
            let (tx, rx) = mpsc::channel(8192);
            let rd = sock.clone();
            tokio::spawn(async move {
                loop {
                    let r = rd.recv().await;
                    let stop = r.is_err();
                    if tx.send(r).await.is_err() || stop {
                        return;
                    }
                }
            });
            Ok(Link { rx, tx: Arc::new(RawSender(sock)) })
        })
    }
    fn kind(&self) -> &'static str {
        "socketcan"
    }
}

pub struct SocketcandOpener {
    pub host: String,
    pub port: u16,
    pub channel: String,
}

/// Sends go through a small queue to the TCP writer task (non-blocking for the caller, like a CAN socket).
struct TcpSender(Mutex<Option<mpsc::Sender<String>>>);

impl Sender for TcpSender {
    fn send(&self, f: &CanFrame) -> io::Result<()> {
        let g = self.0.lock().unwrap();
        match g.as_ref() {
            Some(t) => t.try_send(send_line(f)).map_err(|_| io::Error::from_raw_os_error(libc::ENOBUFS)),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "socketcand link closed")),
        }
    }
}

impl Opener for SocketcandOpener {
    fn open(&self) -> BoxFut<io::Result<Link>> {
        let (h, p, c) = (self.host.clone(), self.port, self.channel.clone());
        Box::pin(async move {
            let (mut sc, mut wr) = Socketcand::connect(&h, p, &c).await?;
            let (tx, rx) = mpsc::channel(8192);
            let (stx, mut srx) = mpsc::channel::<String>(64);
            let sender = Arc::new(TcpSender(Mutex::new(Some(stx))));
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                while let Some(line) = srx.recv().await {
                    if wr.write_all(line.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
            let s2 = sender.clone();
            tokio::spawn(async move {
                loop {
                    match sc.next_msg().await {
                        Ok(m) => {
                            if let Some(mut f) = parse(&m) {
                                f.ts = aetherrv_core::clock::now_s();
                                if tx.send(Ok(f)).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            break;
                        }
                    }
                }
                *s2.0.lock().unwrap() = None;
            });
            Ok(Link { rx, tx: sender })
        })
    }
    fn kind(&self) -> &'static str {
        "socketcand"
    }
}
