//! A PMTiles archive on a server, read by HTTP range requests (1089).
//!
//! Every read is one `GET` with a `Range` header; a server that answers
//! `200` with the whole file instead of `206` with the range is refused,
//! since that would download the planet. TLS is rustls on ring with the
//! webpki roots, so a musl-static binary needs no system certificate store.
//!
//! [`daily`] finds the newest Protomaps daily build that answers, as
//! `pmtiles extract` was pointed at by hand in 1062.

use std::time::Duration;

use crate::pmtiles::RangeRead;
use crate::Res;

/// The Protomaps daily builds: `<base><yyyymmdd>.pmtiles`.
pub const PROTOMAPS_BUILDS: &str = "https://build.protomaps.com/";
/// How many days back [`daily`] looks for a build.
pub const DAILY_LOOKBACK: u32 = 10;
/// One request's whole budget: connect, send, and read the range.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Why a read failed, as far as the caller acts on it.
#[derive(Debug)]
pub enum HttpError {
    /// The host did not resolve or the connection was refused or timed
    /// out: no internet, or the server is down.
    Unreachable(String),
    /// The server answered with this status.
    Status(u16),
    /// The server ignored the range: what it sent instead.
    NoRanges(String),
    /// Anything else on the way.
    Other(String),
}

impl core::fmt::Display for HttpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "unreachable: {e}"),
            Self::Status(s) => write!(f, "HTTP status {s}"),
            Self::NoRanges(what) => {
                write!(f, "the server does not answer range requests: {what}")
            }
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<ureq::Error> for HttpError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::StatusCode(s) => Self::Status(s),
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => {
                Self::Unreachable(e.to_string())
            }
            ureq::Error::Io(io) => match io.kind() {
                std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::HostUnreachable
                | std::io::ErrorKind::NetworkUnreachable
                | std::io::ErrorKind::NetworkDown => Self::Unreachable(io.to_string()),
                _ => Self::Other(io.to_string()),
            },
            other => Self::Other(other.to_string()),
        }
    }
}

/// Whether `e` says the source could not be reached at all.
pub fn unreachable(e: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        e.downcast_ref::<HttpError>(),
        Some(HttpError::Unreachable(_))
    )
}

/// An archive at a URL.
pub struct HttpRange {
    agent: ureq::Agent,
    url: String,
}

impl HttpRange {
    pub fn new(url: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("leviculum-mapcut/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            agent,
            url: url.to_owned(),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn get(&self, offset: u64, len: u64) -> Result<Vec<u8>, HttpError> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let range = format!("bytes={offset}-{}", offset + len - 1);
        let mut response = self.agent.get(&self.url).header("Range", &range).call()?;
        match response.status().as_u16() {
            206 => {}
            // A server without ranges sends the whole file: taken for the
            // first read only while the file is no longer than asked.
            200 if offset == 0 => {}
            200 => return Err(HttpError::NoRanges(format!("200 for {range}"))),
            s => return Err(HttpError::Status(s)),
        }
        // One byte over the range: ureq's limit refuses the read that
        // would find the end once the limit is reached, and a body longer
        // than the range is a server that ignored it.
        let body = response
            .body_mut()
            .with_config()
            .limit(len + 1)
            .read_to_vec()
            .map_err(|e| match e {
                ureq::Error::BodyExceedsLimit(_) => {
                    HttpError::NoRanges(format!("more than {range}"))
                }
                e => HttpError::from(e),
            })?;
        if body.len() as u64 > len {
            return Err(HttpError::NoRanges(format!(
                "{} bytes for {range}",
                body.len()
            )));
        }
        Ok(body)
    }
}

impl RangeRead for HttpRange {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        Ok(self.get(offset, len)?)
    }
}

/// The civil date of `days` since 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The URL of the daily build under `base` dated `days` since the epoch.
pub fn daily_url(base: &str, days: i64) -> String {
    let (y, m, d) = civil(days);
    format!("{base}{y:04}{m:02}{d:02}.pmtiles")
}

/// The newest daily build under `base` that answers, looking back
/// [`DAILY_LOOKBACK`] days from `now_unix`. An unreachable host ends the
/// search at once with that error; a missing day tries the day before.
pub fn daily(base: &str, now_unix: i64) -> Res<HttpRange> {
    let today = now_unix.div_euclid(86_400);
    let mut last: Option<HttpError> = None;
    for back in 0..i64::from(DAILY_LOOKBACK) {
        let source = HttpRange::new(&daily_url(base, today - back));
        match source.get(0, 127) {
            Ok(b) if b.starts_with(b"PMTiles") => return Ok(source),
            Ok(_) => last = Some(HttpError::Other("not a PMTiles archive".into())),
            Err(e @ HttpError::Unreachable(_)) => return Err(Box::new(e)),
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => format!("no daily build in {DAILY_LOOKBACK} days under {base}: {e}").into(),
        None => format!("no daily build under {base}").into(),
    })
}
