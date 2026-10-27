//! Outbound URL policy (SSRF protection).
//!
//! By default sources refuse URLs that point to loopback, private, link-local
//! or other non-public addresses, so chat users cannot make the bot probe the
//! host's internal network. `allow_private_networks` lifts this for setups
//! that stream from a LAN.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkPolicy {
    pub allow_private_networks: bool,
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
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
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || (a == 192 && b == 0 && ip.octets()[2] == 0) // IETF protocol assignments
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    // Only global unicast (2000::/3) is public; everything else (including the IPv4-compatible
    // ::/96 and deprecated site-local ranges) is not.
    if first & 0xe000 != 0x2000 {
        return false;
    }
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00 // unique local
        || (first & 0xffc0) == 0xfe80 // link local
        || first == 0x2001 && ip.segments()[1] == 0x0db8 // documentation
        || first == 0x2002 // 6to4 (embeds IPv4)
        || (first == 0x2001 && ip.segments()[1] == 0x0000) // Teredo (embeds IPv4)
        || (first == 0x2001 && (ip.segments()[1] & 0xfff0) == 0x0010)) // ORCHIDv2
}

impl NetworkPolicy {
    /// Validates `url` and returns its normalised form. Callers must pass on **only** the
    /// returned string: tools parse URLs differently (e.g. `http://a\@b/`), so the raw input
    /// may point elsewhere than what was checked. The egress proxy re-checks every connection.
    pub async fn check(&self, url: &str) -> anyhow::Result<String> {
        let authority_end = url.find("://").map(|i| i + 3).unwrap_or(0);
        let authority = url[authority_end..].split(['/', '?', '#']).next().unwrap_or("");
        if url.contains('\\')
            || authority.contains('@')
            || !url.is_ascii()
            || url.chars().any(char::is_control)
        {
            bail!("unsupported URL");
        }
        let parsed = url::Url::parse(url).map_err(|_| anyhow!("invalid URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("only http and https URLs are supported");
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            bail!("URLs with credentials are not supported");
        }
        let host = parsed.host_str().ok_or_else(|| anyhow!("URL has no host"))?;
        if !self.allow_private_networks {
            let port = parsed.port_or_known_default().unwrap_or(80);
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
                Ok(ip) => vec![ip],
                Err(_) => tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|_| anyhow!("cannot resolve host '{host}'"))?
                    .map(|a| a.ip())
                    .collect(),
            };
            if addrs.is_empty() || !addrs.into_iter().all(is_public_ip) {
                bail!("URLs pointing to private or local networks are not allowed");
            }
        }
        Ok(parsed.to_string())
    }

    /// Synchronous check for redirect targets (IP literals and `localhost` only).
    pub fn allows_redirect(&self, url: &url::Url) -> bool {
        if self.allow_private_networks {
            return true;
        }
        match url.host() {
            Some(url::Host::Ipv4(ip)) => is_public_v4(ip),
            Some(url::Host::Ipv6(ip)) => is_public_ip(IpAddr::V6(ip)),
            Some(url::Host::Domain(d)) => !(d.eq_ignore_ascii_case("localhost") || d.ends_with(".localhost")),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocks_private_targets() {
        let policy = NetworkPolicy::default();
        for url in [
            "http://127.0.0.1:8087/api",
            "http://localhost/",
            "http://10.1.2.3/",
            "http://192.168.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "file:///etc/passwd",
            "ftp://example.com/",
            "http://93.184.215.14\\@127.0.0.1:8087/x.mp3",
            "http://user@93.184.215.14/",
            "http://93.184.215.14@127.0.0.1/",
            "http://[64:ff9b::7f00:1]/",
            "http://[2002:7f00:1::]/",
            "http://192.0.0.8/",
            "http://[::127.0.0.1]/",
            "http://[::ffff:0:127.0.0.1]/",
            "http://[fec0::1]/",
            "http://[2001::1]/",
            "http://[100::1]/",
        ] {
            assert!(policy.check(url).await.is_err(), "{url} must be blocked");
        }
        assert_eq!(
            policy.check("HTTP://93.184.215.14/a b").await.unwrap(),
            "http://93.184.215.14/a%20b"
        );
        let open = NetworkPolicy {
            allow_private_networks: true,
        };
        assert!(open.check("http://192.168.0.1/").await.is_ok());
        assert!(open.check("file:///etc/passwd").await.is_err());
    }
}
