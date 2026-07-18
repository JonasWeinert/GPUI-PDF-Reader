use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Returns whether an address is suitable for direct public-internet access.
///
/// This deliberately rejects documentation, benchmarking, transition,
/// multicast, local, and otherwise reserved ranges. It is more restrictive
/// than merely checking `is_private`.
#[must_use]
pub fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _fourth] = address.octets();
    !(first == 0
        || first == 10
        || first == 127
        || (first == 100 && (64..=127).contains(&second))
        || (first == 169 && second == 254)
        || (first == 172 && (16..=31).contains(&second))
        || (first == 192 && second == 0 && third == 0)
        || (first == 192 && second == 0 && third == 2)
        || (first == 192 && second == 88 && third == 99)
        || (first == 192 && second == 168)
        || (first == 198 && (second == 18 || second == 19))
        || (first == 198 && second == 51 && third == 100)
        || (first == 203 && second == 0 && third == 113)
        || first >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }

    let segments = address.segments();
    let global_unicast = (segments[0] & 0xe000) == 0x2000;
    let protocol_assignments = segments[0] == 0x2001 && segments[1] < 0x0200;
    let documentation = (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0);
    let deprecated_six_to_four = segments[0] == 0x2002;

    global_unicast && !protocol_assignments && !documentation && !deprecated_six_to_four
}

pub(crate) fn is_localhost_name(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host
            .to_ascii_lowercase()
            .strip_suffix(".localhost")
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ipv4_local_reserved_and_documentation_ranges() {
        for address in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv6_local_transition_and_documentation_ranges() {
        for address in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "2002:0808:0808::1",
            "3fff::1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn recognizes_localhost_names_without_suffix_confusion() {
        assert!(is_localhost_name("localhost"));
        assert!(is_localhost_name("api.LOCALHOST."));
        assert!(!is_localhost_name("localhost.example"));
        assert!(!is_localhost_name("notlocalhost"));
    }
}
