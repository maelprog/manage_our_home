//! Carrying the browser's address into the internal call to apps/api.
//!
//! apps/api is the security boundary and the only place that knows how a
//! login ended, so the per-(address, email) lock of #178 lives there. But
//! apps/api is reached over the internal Docker network: without help, every
//! `POST /auth/login` it sees comes from this container, and a lock keyed on
//! that address would be one global lock for the whole household.
//!
//! So the SSR layer does what a reverse proxy does — it passes on the
//! `X-Forwarded-For` it received and **appends the address it saw**. apps/api
//! then reads the chain from the right, skipping the hops it trusts
//! (`manage_our_home::client_ip`), and everything this file forwards from the
//! browser sits to the *left* of the address this file appends. A caller who
//! writes their own header therefore cannot pass themselves off as anyone:
//! their invention is behind the address they actually connected from.

use std::net::IpAddr;

/// Header name, as both axum and reqwest spell it.
pub const FORWARDED_FOR: &str = "x-forwarded-for";

/// Hops kept in the forwarded chain, including the one appended here.
///
/// The incoming value is caller-chosen and could be thousands of entries;
/// relaying it whole would mean carrying an attacker-sized header on every
/// internal call. Trimming from the **left** is safe by construction: the
/// reader walks from the right, so the entries dropped are the ones furthest
/// from — and least trusted by — the service that reads them.
pub const MAX_HOPS: usize = 8;

/// The `X-Forwarded-For` to send on the internal call: what came in, plus
/// the address this request actually arrived from.
pub fn forwarded_for(incoming: Option<&str>, peer: IpAddr) -> String {
    let mut hops: Vec<&str> = incoming
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    let peer = peer.to_string();
    hops.push(&peer);
    let start = hops.len().saturating_sub(MAX_HOPS);
    hops[start..].join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn with_no_incoming_header_the_peer_is_the_whole_chain() {
        assert_eq!(forwarded_for(None, ip("192.168.1.42")), "192.168.1.42");
    }

    #[test]
    fn the_peer_is_appended_to_the_right_of_what_came_in() {
        // Rightmost is what *we* observed; apps/api reads from that end.
        assert_eq!(
            forwarded_for(Some("192.168.1.42"), ip("172.18.0.2")),
            "192.168.1.42, 172.18.0.2"
        );
    }

    #[test]
    fn an_empty_or_blank_incoming_header_contributes_nothing() {
        assert_eq!(forwarded_for(Some(""), ip("172.18.0.2")), "172.18.0.2");
        assert_eq!(forwarded_for(Some("  , ,"), ip("172.18.0.2")), "172.18.0.2");
    }

    #[test]
    fn spacing_is_normalised_so_the_reader_never_has_to_guess() {
        assert_eq!(
            forwarded_for(Some("8.8.8.8,   192.168.1.42  "), ip("172.18.0.2")),
            "8.8.8.8, 192.168.1.42, 172.18.0.2"
        );
    }

    #[test]
    fn whatever_the_caller_invented_stays_left_of_the_address_we_saw() {
        let forwarded = forwarded_for(Some("203.0.113.9"), ip("192.168.1.42"));
        let hops: Vec<&str> = forwarded.split(", ").collect();
        assert_eq!(hops.last(), Some(&"192.168.1.42"));
        assert_eq!(hops[0], "203.0.113.9");
    }

    #[test]
    fn an_oversized_chain_is_trimmed_from_the_left_and_keeps_our_hop() {
        let incoming = (0..50)
            .map(|i| format!("198.51.100.{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let forwarded = forwarded_for(Some(&incoming), ip("172.18.0.2"));
        let hops: Vec<&str> = forwarded.split(", ").collect();

        assert_eq!(hops.len(), MAX_HOPS);
        assert_eq!(hops.last(), Some(&"172.18.0.2"));
        // The survivors are the ones nearest the reader's end: the last
        // seven of the fifty that came in, then our own hop.
        assert_eq!(hops[0], "198.51.100.43");
        assert_eq!(hops[6], "198.51.100.49");
    }
}
