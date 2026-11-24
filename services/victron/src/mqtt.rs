//! The MQTT connection to the Cerbo (Python `VictronAdapter.run()`): MQTT 3.1.1, client id `aetherrv`, clean
//! session, QoS 0, keepalive 60 s (aiomqtt's defaults), plain on 1883 or TLS on 8883 with the GX's self-signed
//! certificate (not verified: LAN only, as Python's `CERT_NONE`).
//!
//! Each connection is a new client, as Python's `async with aiomqtt.Client(...)`: when it ends, its request queue
//! goes with it, so a `W/` queued on a dead connection is never sent after a reconnect.

use crate::{Link, Settings, Victron};
use rumqttc::tokio_rustls::rustls;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use std::sync::Arc;
use std::time::Duration;

/// The Python client's identifier.
pub const CLIENT_ID: &str = "aetherrv";
/// aiomqtt's connect timeout (paho's default 10 s).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Venus topics are small, but `system/0/...` carries a few JSON lists; rumqttc's 10 KiB default would drop the link.
const MAX_PACKET: usize = 1 << 20;

/// A rumqttc client as a [`Link`]: requests are queued without waiting (never awaited inside the poll loop).
pub struct MqttLink(pub AsyncClient);

impl Link for MqttLink {
    fn subscribe(&self, filter: &str) -> Result<(), String> {
        self.0.try_subscribe(filter, QoS::AtMostOnce).map_err(|e| e.to_string())
    }
    fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), String> {
        self.0.try_publish(topic, QoS::AtMostOnce, false, payload.to_vec()).map_err(|e| e.to_string())
    }
}

/// The client options for one connection.
pub fn options(s: &Settings, client_id: &str) -> MqttOptions {
    let mut mo = MqttOptions::new(client_id, s.host.clone(), s.port);
    mo.set_keep_alive(Duration::from_secs(60));
    mo.set_clean_session(true);
    mo.set_max_packet_size(MAX_PACKET, MAX_PACKET);
    // Python passes the password only together with a username (aiomqtt `username_pw_set`)
    if let Some(u) = &s.username {
        mo.set_credentials(u.clone(), s.password.clone().unwrap_or_default());
    }
    if s.tls {
        mo.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(Arc::new(insecure_tls()))));
    }
    mo
}

/// Run connections forever: 2 s backoff doubling to 60 s, reset by a successful connect.
pub async fn connection_loop(v: Arc<Victron>, s: Settings) {
    let mut backoff = 2.0f64;
    loop {
        if let Err(e) = connection(&v, &s, CLIENT_ID, &mut backoff).await {
            tracing::warn!("victron mqtt {}:{}: {e}", s.host, s.port);
        }
        v.on_disconnect();
        tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
        backoff = (backoff * 2.0).min(60.0);
    }
}

/// Aborts the per-connection tasks when the connection ends (Python cancels `ka` and `fb`).
struct Tasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// One connection's life; returns the error that ended it.
pub async fn connection(v: &Arc<Victron>, s: &Settings, client_id: &str, backoff: &mut f64) -> anyhow::Result<()> {
    let (client, mut ev) = AsyncClient::new(options(s, client_id), 1024);
    loop {
        match tokio::time::timeout(CONNECT_TIMEOUT, ev.poll()).await {
            Err(_) => anyhow::bail!("connect timed out"),
            Ok(Err(e)) => return Err(e.into()),
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => break,
            Ok(Ok(_)) => {}
        }
    }
    let link: Arc<MqttLink> = Arc::new(MqttLink(client));
    v.on_connect(link.clone());
    *backoff = 2.0;
    let ka = {
        let (v, link) = (v.clone(), link.clone());
        tokio::spawn(async move {
            loop {
                v.keepalive(&*link);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        })
    };
    let fb = {
        let v = v.clone();
        tokio::spawn(async move {
            loop {
                v.flush_batteries();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    };
    let _tasks = Tasks(vec![ka, fb]);
    loop {
        match ev.poll().await {
            Ok(Event::Incoming(Packet::Publish(p))) => v.on_message(&*link, &p.topic, &p.payload),
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// TLS without certificate verification (the GX's self-signed certificate; Python `CERT_NONE`). The handshake
/// signatures are still checked against the certificate the GX presents.
pub fn insecure_tls() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("rustls default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth()
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_options_build_without_a_process_provider() {
        let _ = insecure_tls();
        let s = Settings {
            host: "192.0.2.10".into(),
            port: 8883,
            tls: true,
            username: Some("token/x".into()),
            password: Some("secret".into()),
            roles: vec![],
        };
        let o = options(&s, CLIENT_ID);
        assert_eq!(o.broker_address(), ("192.0.2.10".to_string(), 8883));
        assert!(!format!("{s:?}").contains("secret") && !format!("{s:?}").contains("token/x"));
    }
}
