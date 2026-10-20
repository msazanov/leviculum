//! Host-name resolution for outbound TCP dials.
//!
//! A TCP client resolves its target on every connect attempt, not once when
//! the interface is built (Codeberg #461): a name that does not resolve yet
//! is a failed attempt the reconnect loop retries, as it is in the reference
//! (`TCPInterface.py`, `getaddrinfo` inside `connect`). The node uses the
//! system resolver unless an embedder registers a [`HostResolver`] via
//! [`ReticulumNodeBuilder::host_resolver`](crate::driver::ReticulumNodeBuilder::host_resolver),
//! for instance one that asks a DNS service the host manages, or a test
//! that scripts the answers.

use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

/// Resolves `(host, port)` to the addresses to dial, in the order to try
/// them. Called once per connect attempt, off the async runtime (it may
/// block). An empty answer counts as a failed lookup.
pub type HostResolver = Arc<dyn Fn(&str, u16) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

/// Resolve `host:port` with `resolver`, or the system resolver when `None`.
///
/// An IP literal never reaches a resolver, custom or system: it is the
/// address. Everything else runs on the blocking pool, since `getaddrinfo`
/// and an embedder's resolver may both block.
pub(crate) async fn resolve(
    resolver: Option<&HostResolver>,
    host: &str,
    port: u16,
) -> io::Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let owned = host.to_string();
    let resolver = resolver.cloned();
    let addrs = tokio::task::spawn_blocking(move || match resolver {
        Some(resolve) => resolve(&owned, port),
        None => (owned.as_str(), port)
            .to_socket_addrs()
            .map(|it| it.collect()),
    })
    .await
    .map_err(|e| io::Error::other(format!("resolver task failed: {e}")))??;
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no addresses for {host}:{port}"),
        ));
    }
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_ip_literal_skips_the_resolver() {
        let refuse: HostResolver = Arc::new(|_, _| Err(io::Error::other("must not be called")));
        let v4 = resolve(Some(&refuse), "127.0.0.1", 7).await.unwrap();
        assert_eq!(v4, vec!["127.0.0.1:7".parse().unwrap()]);
        let v6 = resolve(Some(&refuse), "::1", 7).await.unwrap();
        assert_eq!(v6, vec!["[::1]:7".parse().unwrap()]);
    }

    #[tokio::test]
    async fn an_empty_answer_is_a_failed_lookup() {
        let empty: HostResolver = Arc::new(|_, _| Ok(Vec::new()));
        let err = resolve(Some(&empty), "peer.invalid", 7).await.unwrap_err();
        assert!(
            err.to_string().starts_with("no addresses for peer"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_resolver_error_is_returned_verbatim() {
        let refuse: HostResolver = Arc::new(|_, _| Err(io::Error::other("scripted refusal")));
        let err = resolve(Some(&refuse), "peer.invalid", 7).await.unwrap_err();
        assert_eq!(err.to_string(), "scripted refusal");
    }
}
