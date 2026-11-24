//! The outgoing HTTP side behind one small trait, so the recorder and the traffic proxy run against fakes in tests
//! (never the internet).
//!
//! A failed fetch is reported as a class name only ([`FetchError`]), never with its URL: a TomTom URL carries the
//! API key, and `reqwest::Error`'s `Display` prints the full URL.

use hyper::body::Bytes;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// An upstream answer (any status).
#[derive(Debug, Clone)]
pub struct Fetched {
    pub status: u16,
    pub body: Bytes,
}

/// Why a fetch failed, as the class name the Python logs (`type(e).__name__` of the aiohttp exception). Holds no
/// URL by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchError(pub &'static str);

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for FetchError {}

/// GET a URL.
pub trait Fetch: Send + Sync {
    fn get(&self, url: &str) -> BoxFuture<Result<Fetched, FetchError>>;
}

/// The real client (reqwest, rustls). `timeout` covers the whole request including the body, like aiohttp's
/// `ClientTimeout(total=…)`.
pub struct HttpFetch {
    client: reqwest::Client,
}

impl HttpFetch {
    pub fn new(user_agent: Option<&str>, timeout: Duration) -> anyhow::Result<HttpFetch> {
        let mut b = reqwest::Client::builder().timeout(timeout);
        if let Some(ua) = user_agent {
            b = b.user_agent(ua);
        }
        // the builder's error can't contain a request URL (none exists yet)
        Ok(HttpFetch { client: b.build()? })
    }
}

/// The aiohttp class of a reqwest error (for the log lines), without the URL.
fn class(e: &reqwest::Error) -> FetchError {
    FetchError(if e.is_timeout() {
        "TimeoutError"
    } else if e.is_connect() {
        "ClientConnectorError"
    } else if e.is_redirect() {
        "TooManyRedirects"
    } else if e.is_body() || e.is_decode() {
        "ClientPayloadError"
    } else {
        "ClientError"
    })
}

impl Fetch for HttpFetch {
    fn get(&self, url: &str) -> BoxFuture<Result<Fetched, FetchError>> {
        let rq = self.client.get(url);
        Box::pin(async move {
            let r = rq.send().await.map_err(|e| class(&e))?;
            let status = r.status().as_u16();
            let body = r.bytes().await.map_err(|e| class(&e))?;
            Ok(Fetched { status, body })
        })
    }
}
