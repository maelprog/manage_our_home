//! Who is on the other end of the request, behind the reverse proxies.
//!
//! Until #178 this API had no notion of a client address at all: it is
//! reached over the internal Docker network, so every `POST /auth/login`
//! arrives from the `web` container (or from Caddy, on the `/api/*` path
//! `infra/Caddyfile` exposes). Anything keyed on the peer address alone
//! would therefore key on *one* address for the whole world — a per-IP
//! login throttle built that way locks the entire household out the first
//! time anyone attacks it.
//!
//! The address the household actually browses from only exists in
//! `X-Forwarded-For`, and that header is attacker-controlled: it travels in
//! a request the attacker composes. **Reading it without a list of trusted
//! peers is worse than not reading it at all** — a client that can set it
//! freely escapes any throttle by rotating the value, and can pin a
//! neighbour's address to lock *them* out.
//!
//! So [`resolve`] honours the header under one condition and reads it from
//! one end:
//!
//! - the immediate peer must itself be a trusted proxy, otherwise the
//!   header is ignored entirely and the peer address is the client;
//! - the entry taken is the **rightmost one that is not itself a trusted
//!   proxy**, never the leftmost. Each proxy in the chain appends the
//!   address it saw, so the entries on the right are the ones our own
//!   infrastructure wrote; everything further left was supplied by the
//!   caller and may be pure invention.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use ipnet::IpNet;

use crate::AppState;

/// The header [`resolve`] reads, lowercase as `http` stores it.
pub const FORWARDED_FOR: &str = "x-forwarded-for";

/// Environment variable holding the comma-separated CIDR list of peers
/// whose `X-Forwarded-For` is believed.
pub const TRUSTED_PROXIES_VAR: &str = "TRUSTED_PROXY_CIDRS";

/// Trusted by default: loopback, and the `172.16.0.0/12` block Docker
/// allocates its bridge networks from — which is exactly where `caddy`,
/// `web` and `api` sit in `infra/docker-compose.yml`, and nothing else can
/// reach `api` there (it publishes no port; only `caddy` does).
///
/// A deployment whose *host* LAN overlaps that block must narrow this with
/// [`TRUSTED_PROXIES_VAR`] to the compose network's own subnet: a trusted
/// address is one whose `X-Forwarded-For` claims are taken at face value.
pub const DEFAULT_TRUSTED_PROXY_CIDRS: &str = "127.0.0.0/8,::1/128,172.16.0.0/12";

/// The peers whose `X-Forwarded-For` is honoured.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    /// Parses a comma-separated list of CIDR blocks. A bare address is
    /// accepted and means that address alone (`/32`, `/128`).
    ///
    /// Returns the offending entry rather than silently dropping it: a
    /// typo that quietly shrinks the trust list turns every client into
    /// one throttle key, and a typo that quietly *grows* it hands the
    /// header to whoever is in the widened range. Neither may pass
    /// unnoticed, so the caller (`main`) refuses to start.
    pub fn parse(list: &str) -> Result<Self, String> {
        let mut nets = Vec::new();
        for entry in list.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let net = entry
                .parse::<IpNet>()
                .or_else(|_| entry.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| format!("not an IP address or CIDR block: {entry:?}"))?;
            nets.push(net);
        }
        Ok(Self(nets))
    }

    /// Reads [`TRUSTED_PROXIES_VAR`], falling back to
    /// [`DEFAULT_TRUSTED_PROXY_CIDRS`] when it is unset or empty.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var(TRUSTED_PROXIES_VAR) {
            Ok(v) if !v.trim().is_empty() => Self::parse(&v),
            _ => Self::parse(DEFAULT_TRUSTED_PROXY_CIDRS),
        }
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(&ip))
    }

    /// Trusts nobody: every `X-Forwarded-For` is ignored and the peer
    /// address is the client. What integration tests get, and the only
    /// safe posture for a process reached directly by browsers.
    pub fn none() -> Self {
        Self(Vec::new())
    }
}

/// The client address behind the proxy chain.
///
/// `peer` is the address the socket came from, `None` when the server was
/// started without `into_make_service_with_connect_info` (integration
/// tests calling the router directly). With no peer there is nothing to
/// trust, so the header is ignored and the unspecified address stands in:
/// one bucket for everyone, which is the conservative answer when the
/// deployment cannot say who is calling.
pub fn resolve(
    peer: Option<IpAddr>,
    forwarded_for: Option<&str>,
    trusted: &TrustedProxies,
) -> IpAddr {
    let Some(peer) = peer else {
        return IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    };
    if !trusted.contains(peer) {
        return peer;
    }
    let Some(header) = forwarded_for else {
        return peer;
    };

    // Right to left: the rightmost entry was appended by our own immediate
    // peer, the next one by the hop before it, and so on. Skipping trusted
    // addresses walks back out through our infrastructure; the first
    // address that is not ours is the client that reached its edge.
    let mut innermost_trusted = None;
    for token in header.split(',').rev() {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        match parse_entry(token) {
            Some(ip) if trusted.contains(ip) => innermost_trusted = Some(ip),
            Some(ip) => return ip,
            // Not an address at all — RFC 7239's `unknown`, an obfuscated
            // identifier, or junk the caller inserted. It cannot be a hop
            // our infrastructure wrote, so the walk stops here and keeps
            // the innermost *trusted* address it did see: the chain is
            // only trustworthy up to this point.
            None => break,
        }
    }
    // Every entry was one of ours (or the chain ended in junk): the caller
    // reached the edge from inside the trusted range, so the leftmost
    // trusted entry is as close to the client as this header gets.
    innermost_trusted.unwrap_or(peer)
}

/// One `X-Forwarded-For` entry. Conformant proxies write a bare address,
/// but `host:port` and `[v6]:port` forms are common enough in the wild
/// (and `Forwarded`'s `for=` syntax uses them) that dropping the port is
/// worth the four lines.
fn parse_entry(token: &str) -> Option<IpAddr> {
    if let Ok(ip) = token.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Ok(addr) = token.parse::<SocketAddr>() {
        return Some(addr.ip());
    }
    // `[::1]` without a port.
    let inner = token.strip_prefix('[')?.strip_suffix(']')?;
    inner.parse::<IpAddr>().ok()
}

/// Extractor form of [`resolve`], reading the trust list off [`AppState`].
///
/// Infallible on purpose: a handler that needs the client address needs an
/// answer, not a rejection, and [`resolve`] always has one.
pub struct ClientIp(pub IpAddr);

#[axum::async_trait]
impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip());
        let forwarded = parts
            .headers
            .get(FORWARDED_FOR)
            .and_then(|v| v.to_str().ok());
        Ok(ClientIp(resolve(peer, forwarded, &state.trusted_proxies)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn docker() -> TrustedProxies {
        TrustedProxies::parse(DEFAULT_TRUSTED_PROXY_CIDRS).unwrap()
    }

    #[test]
    fn the_default_list_covers_the_compose_network_and_loopback() {
        let t = docker();
        assert!(t.contains(ip("172.18.0.4")), "docker bridge range");
        assert!(t.contains(ip("127.0.0.1")));
        assert!(t.contains(ip("::1")));
        assert!(
            !t.contains(ip("192.168.1.10")),
            "household LAN is not a proxy"
        );
        assert!(!t.contains(ip("8.8.8.8")));
    }

    #[test]
    fn a_bare_address_in_the_list_means_that_address_alone() {
        let t = TrustedProxies::parse("10.1.2.3, ::1").unwrap();
        assert!(t.contains(ip("10.1.2.3")));
        assert!(!t.contains(ip("10.1.2.4")));
        assert!(t.contains(ip("::1")));
    }

    #[test]
    fn a_malformed_entry_is_reported_rather_than_dropped() {
        let err = TrustedProxies::parse("10.0.0.0/8, nonsense").unwrap_err();
        assert!(err.contains("nonsense"), "{err}");
    }

    #[test]
    fn an_untrusted_peer_gets_its_forwarded_for_ignored() {
        // The whole point: a client that reaches the process directly
        // cannot name itself something else.
        let got = resolve(Some(ip("203.0.113.9")), Some("8.8.8.8"), &docker());
        assert_eq!(got, ip("203.0.113.9"));
    }

    #[test]
    fn a_trusted_peer_with_no_header_is_the_client() {
        let got = resolve(Some(ip("172.18.0.4")), None, &docker());
        assert_eq!(got, ip("172.18.0.4"));
    }

    #[test]
    fn the_browser_address_is_read_through_caddy_and_web() {
        // caddy appended the browser's address, web appended caddy's.
        let got = resolve(
            Some(ip("172.18.0.5")),
            Some("192.168.1.42, 172.18.0.2"),
            &docker(),
        );
        assert_eq!(got, ip("192.168.1.42"));
    }

    #[test]
    fn a_forged_prefix_never_wins_over_what_the_proxies_appended() {
        // Exactly the attack the leftmost reading would fall for: the
        // client sent `X-Forwarded-For: 8.8.8.8` and caddy appended the
        // address it really saw.
        let got = resolve(
            Some(ip("172.18.0.5")),
            Some("8.8.8.8, 1.1.1.1, 192.168.1.42, 172.18.0.2"),
            &docker(),
        );
        assert_eq!(got, ip("192.168.1.42"));
    }

    #[test]
    fn a_chain_made_only_of_trusted_hops_falls_back_to_its_leftmost_entry() {
        // No untrusted hop anywhere: the caller reached the edge from
        // inside the trusted range (an e2e stack where the browser itself
        // is a container on the same network).
        let got = resolve(
            Some(ip("172.18.0.5")),
            Some("172.18.0.9, 172.18.0.2"),
            &docker(),
        );
        assert_eq!(got, ip("172.18.0.9"));
    }

    #[test]
    fn junk_in_the_chain_stops_the_walk_instead_of_being_believed() {
        let got = resolve(
            Some(ip("172.18.0.5")),
            Some("8.8.8.8, unknown, 172.18.0.2"),
            &docker(),
        );
        assert_eq!(got, ip("172.18.0.2"), "the last hop we actually wrote");
    }

    #[test]
    fn junk_with_no_trusted_hop_behind_it_falls_back_to_the_peer() {
        let got = resolve(Some(ip("172.18.0.5")), Some("unknown"), &docker());
        assert_eq!(got, ip("172.18.0.5"));
    }

    #[test]
    fn an_empty_header_falls_back_to_the_peer() {
        let got = resolve(Some(ip("172.18.0.5")), Some("   ,  "), &docker());
        assert_eq!(got, ip("172.18.0.5"));
    }

    #[test]
    fn entries_carrying_a_port_are_read_without_it() {
        assert_eq!(
            resolve(
                Some(ip("172.18.0.5")),
                Some("192.168.1.42:51321, 172.18.0.2"),
                &docker()
            ),
            ip("192.168.1.42")
        );
        assert_eq!(
            resolve(
                Some(ip("172.18.0.5")),
                Some("[2001:db8::1]:443, 172.18.0.2"),
                &docker()
            ),
            ip("2001:db8::1")
        );
        assert_eq!(
            resolve(
                Some(ip("172.18.0.5")),
                Some("[2001:db8::1], 172.18.0.2"),
                &docker()
            ),
            ip("2001:db8::1")
        );
    }

    #[test]
    fn no_peer_at_all_ignores_the_header_entirely() {
        // Integration tests drive the router without a socket. Believing
        // the header there would mean believing it in any deployment that
        // forgot `into_make_service_with_connect_info` — the one case
        // where nothing has been vouched for.
        let got = resolve(None, Some("8.8.8.8"), &docker());
        assert_eq!(got, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn an_empty_trust_list_never_honours_the_header() {
        let got = resolve(
            Some(ip("172.18.0.5")),
            Some("8.8.8.8"),
            &TrustedProxies::none(),
        );
        assert_eq!(got, ip("172.18.0.5"));
    }
}
