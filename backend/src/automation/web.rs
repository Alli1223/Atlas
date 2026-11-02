//! The "send web request" action, and the SSRF guard every server-side fetch of a
//! user-supplied URL goes through.
//!
//! # The guard
//!
//! A rule's URL is user input, and Atlas runs inside someone's network. Without a guard,
//! `http://169.254.169.254/latest/meta-data/` (cloud credentials), `http://localhost:8080/api/…`
//! (Atlas itself, from inside), or `http://192.168.1.1/` (the router) are one rule away. So:
//!
//! 1. only `http`/`https`;
//! 2. the host is resolved *here*, and every address it resolves to must be public — not
//!    loopback, private, link-local, CGNAT, multicast, unspecified or documentation space;
//! 3. the request is then pinned to the checked address (`resolve`), so a DNS answer that
//!    changes between the check and the connect (rebinding) cannot redirect it;
//! 4. redirects are not followed — a public URL answering `302 Location: http://127.0.0.1/`
//!    is the oldest way around a resolve-time check.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::{AppError, AppResult};

/// The most response body kept for later actions.
const MAX_BODY: usize = 64 * 1024;

/// Whether an address is safe to reach from the server: public unicast only.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_v4(mapped);
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 unique local
                || (first & 0xfe00) == 0xfc00
                // fe80::/10 link local
                || (first & 0xffc0) == 0xfe80
                // 2001:db8::/32 documentation
                || (first == 0x2001 && v6.segments()[1] == 0x0db8))
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 100.64.0.0/10 carrier-grade NAT
        || (a == 100 && (64..=127).contains(&b))
        // 0.0.0.0/8 "this network"
        || a == 0
        // 198.18.0.0/15 benchmarking
        || (a == 198 && (b == 18 || b == 19))
        // 240.0.0.0/4 reserved
        || a >= 240)
}

/// Resolves `url`'s host and refuses it unless every address is public. Returns the host and
/// the address to pin the connection to.
pub async fn guard(url: &reqwest::Url) -> AppResult<(String, SocketAddr)> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Validation(
            "Only http and https URLs may be requested.".to_owned(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::Validation("The URL has no host.".to_owned()))?
        .to_owned();
    let port = url.port_or_known_default().unwrap_or(443);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let addresses: Vec<SocketAddr> = if let Ok(ip) = bare.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((bare, port))
            .await
            .map_err(|err| AppError::Validation(format!("Could not resolve {host}: {err}")))?
            .collect()
    };
    let first = *addresses
        .first()
        .ok_or_else(|| AppError::Validation(format!("{host} resolved to nothing.")))?;
    if let Some(bad) = addresses.iter().find(|a| !is_public(a.ip())) {
        return Err(AppError::Validation(format!(
            "{host} resolves to {}, a private or local address — refused.",
            bad.ip()
        )));
    }
    Ok((bare.to_owned(), first))
}

/// Sends a request through the guard. Returns `{ status, headers, body }` — the body parsed as
/// JSON when it is JSON, else text, truncated to 64 KiB.
pub async fn send(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<String>,
    timeout: Duration,
) -> AppResult<Value> {
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|_| AppError::Validation(format!("{url:?} is not a URL.")))?;
    let (host, address) = guard(&parsed).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .resolve(&host, address)
        .user_agent(concat!(
            "Atlas/",
            env!("CARGO_PKG_VERSION"),
            " (automation)"
        ))
        .build()
        .map_err(AppError::internal)?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| AppError::Validation(format!("{method:?} is not an HTTP method.")))?;
    let mut request = client.request(method, parsed);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|err| AppError::Validation(format!("The request failed: {err}")))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| AppError::Validation(format!("Reading the response failed: {err}")))?;
    let slice = &bytes[..bytes.len().min(MAX_BODY)];
    let body = serde_json::from_slice::<Value>(slice)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(slice).into_owned()));
    if status >= 400 {
        return Err(AppError::Validation(format!(
            "The server answered {status}."
        )));
    }
    Ok(json!({ "status": status, "body": body }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_loopback_and_metadata_addresses_are_refused() {
        for bad in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.3.4",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(!is_public(ip), "{bad} must be refused");
        }
        for good in ["1.1.1.1", "140.82.112.3", "2606:4700:4700::1111"] {
            let ip: IpAddr = good.parse().unwrap();
            assert!(is_public(ip), "{good} is public");
        }
    }

    #[tokio::test]
    async fn the_guard_refuses_local_hosts_before_any_request() {
        for url in [
            "http://127.0.0.1:8080/api/v1/users",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data/",
            "file:///etc/passwd",
            "http://localhost/",
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert!(guard(&parsed).await.is_err(), "{url} must be refused");
        }
    }
}
