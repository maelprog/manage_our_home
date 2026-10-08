//! Where a request made on behalf of a member may go when the member chose
//! the address (#405, the recipe import): the public Internet, and nothing
//! this server can reach that the member cannot — no loopback, no private
//! or link-local network (cloud metadata at `169.254.169.254` included), no
//! unique-local or otherwise unroutable address.
//!
//! The check is made on the address actually connected to, not on the name
//! the URL gives: `PublicOnlyResolver` is the DNS resolver of the client
//! that makes these requests, and drops every address that is not public,
//! so a name that resolves to `127.0.0.1` — or that resolves to a public
//! address first and to a private one on the next lookup (DNS rebinding) —
//! never reaches a private one. An IP literal in the URL is never handed
//! to a resolver: `check_url` refuses it beforehand, on the first request
//! and on each redirect (`redirect_policy`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::Url;

/// Redirects followed, at most, before giving up.
pub const MAX_REDIRECTS: usize = 5;

/// Why a destination was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Not `http` or `https`.
    Scheme,
    /// No host, or a host that is or resolves only to a non-public address.
    Destination,
    /// More than `MAX_REDIRECTS` redirects.
    TooManyRedirects,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refused::Scheme => "scheme refused",
            Refused::Destination => "destination refused",
            Refused::TooManyRedirects => "too many redirects",
        })
    }
}

impl std::error::Error for Refused {}

/// Whether `ip` is an address of the public Internet.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// IPv4 ranges that are not the public Internet (IANA special-purpose
/// registry): "this network", private (RFC 1918), shared address space
/// (CGNAT), loopback, link-local, IETF protocol assignments, documentation,
/// benchmarking, multicast, reserved and broadcast.
const NON_PUBLIC_V4: [(Ipv4Addr, u8); 14] = [
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let bits = u32::from(ip);
    !NON_PUBLIC_V4.iter().any(|&(net, len)| {
        let mask = u32::MAX << (32 - len);
        bits & mask == u32::from(net) & mask
    })
}

/// IPv6: only global unicast (`2000::/3`) is public, less documentation
/// (`2001:db8::/32`) and the IETF protocol assignments (`2001::/23`, Teredo
/// included). An IPv4 address carried in IPv6 — mapped (`::ffff:0:0/96`),
/// NAT64 (`64:ff9b::/96`) or 6to4 (`2002::/16`) — is judged as that IPv4
/// address; the first two lie outside `2000::/3`, so are only public that
/// way.
fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    let embedded = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_public_v4(embedded(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return is_public_v4(embedded(s[1], s[2]));
    }
    let global_unicast = s[0] & 0xe000 == 0x2000;
    let documentation = s[0] == 0x2001 && s[1] == 0x0db8;
    let protocol_assignments = s[0] == 0x2001 && s[1] < 0x0200;
    global_unicast && !documentation && !protocol_assignments
}

/// Whether `url` may be requested: `http` or `https`, with a host that is
/// a name (left to `PublicOnlyResolver`) or a public IP literal.
pub fn check_url(url: &Url) -> Result<(), Refused> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refused::Scheme);
    }
    // The URL parser has already rewritten every IPv4 spelling it accepts
    // (`2130706433`, `0x7f.1`, `127.1`) as dotted decimal, and an IPv6
    // literal comes bracketed: a host that parses as an address is one.
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or(Refused::Destination)?;
    match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(ip) if !is_public_ip(ip) => Err(Refused::Destination),
        _ => Ok(()),
    }
}

/// The resolver of the client that fetches member-chosen URLs: resolves
/// with the system resolver, then keeps only public addresses. A name left
/// with none fails with `Refused::Destination`.
#[derive(Debug, Default, Clone, Copy)]
pub struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let public: Vec<SocketAddr> = resolved.filter(|a| is_public_ip(a.ip())).collect();
            if public.is_empty() {
                return Err(
                    Box::new(Refused::Destination) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The redirect policy of that client: at most `MAX_REDIRECTS` hops, each
/// one checked by `check_url` (a name is then resolved by
/// `PublicOnlyResolver`, like the first request's).
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error(Refused::TooManyRedirects);
        }
        match check_url(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(refused) => attempt.error(refused),
        }
    })
}

/// The `Refused` somewhere in `error`'s chain of sources, if any: how a
/// refusal by the resolver or the redirect policy comes back out of
/// reqwest.
pub fn refusal_in(error: &(dyn std::error::Error + 'static)) -> Option<Refused> {
    let mut current = Some(error);
    while let Some(e) = current {
        if let Some(refused) = e.downcast_ref::<Refused>() {
            return Some(*refused);
        }
        current = e.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse::<Ipv4Addr>().unwrap())
    }

    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse::<Ipv6Addr>().unwrap())
    }

    #[test]
    fn public_ipv4_addresses_are_public() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.215.14",
            "172.32.0.1",
            "100.128.0.1",
        ] {
            assert!(is_public_ip(v4(ip)), "{ip}");
        }
    }

    #[test]
    fn loopback_private_link_local_and_reserved_ipv4_are_not() {
        for ip in [
            "127.0.0.1",
            "127.255.255.254",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!is_public_ip(v4(ip)), "{ip}");
        }
    }

    #[test]
    fn global_unicast_ipv6_is_public() {
        for ip in ["2606:4700:4700::1111", "2a00:1450:4007:80e::200e"] {
            assert!(is_public_ip(v6(ip)), "{ip}");
        }
    }

    #[test]
    fn loopback_unique_local_link_local_and_reserved_ipv6_are_not() {
        for ip in [
            "::1",
            "::",
            "fc00::1",
            "fd12:3456:789a::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "100::1",
            "64:ff9b::a00:1",
        ] {
            assert!(!is_public_ip(v6(ip)), "{ip}");
        }
    }

    #[test]
    fn an_ipv4_address_carried_in_ipv6_is_judged_as_ipv4() {
        // Mapped (::ffff:a.b.c.d), NAT64 (64:ff9b::/96) and 6to4 (2002::/16).
        assert!(!is_public_ip(v6("::ffff:127.0.0.1")));
        assert!(!is_public_ip(v6("::ffff:10.0.0.1")));
        assert!(!is_public_ip(v6("::ffff:169.254.169.254")));
        assert!(is_public_ip(v6("::ffff:1.1.1.1")));
        assert!(!is_public_ip(v6("64:ff9b::7f00:1")));
        assert!(is_public_ip(v6("64:ff9b::101:101")));
        assert!(!is_public_ip(v6("2002:c0a8:101::1")));
        assert!(is_public_ip(v6("2002:101:101::1")));
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn http_and_https_names_and_public_literals_pass() {
        for u in [
            "https://www.marmiton.org/recettes/recette_crepes_1.aspx",
            "http://example.com:8080/a?b=c",
            "https://1.1.1.1/",
            "https://[2606:4700:4700::1111]/",
        ] {
            assert_eq!(check_url(&url(u)), Ok(()), "{u}");
        }
    }

    #[test]
    fn other_schemes_are_refused() {
        for u in [
            "ftp://example.com/",
            "file:///etc/passwd",
            "gopher://example.com/",
            "data:text/html,<p>x</p>",
            "javascript:alert(1)",
        ] {
            assert_eq!(check_url(&url(u)), Err(Refused::Scheme), "{u}");
        }
    }

    #[test]
    fn non_public_literals_are_refused_whatever_their_spelling() {
        for u in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/recette",
            "http://10.0.0.5/",
            "http://192.168.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://0.0.0.0/",
            // The URL parser reads these as 127.0.0.1.
            "http://2130706433/",
            "http://0x7f.1/",
            "http://127.1/",
        ] {
            assert_eq!(check_url(&url(u)), Err(Refused::Destination), "{u}");
        }
    }
}
