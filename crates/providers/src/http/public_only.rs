//! Addresses a request from a package may reach (#174): public unicast only. A plugin provider's
//! origin is a public DNS name, but the name is the publisher's, and its records can point
//! anywhere; resolving through [`PublicOnly`] keeps a template from reaching the machine itself,
//! its network, or a cloud metadata service.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is a public unicast address.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0                                   // this network
        || a == 10                             // private
        || a == 127                            // loopback
        || (a == 100 && (64..128).contains(&b)) // shared address space (carrier NAT)
        || (a == 169 && b == 254)              // link-local, cloud metadata
        || (a == 172 && (16..32).contains(&b)) // private
        || (a == 192 && b == 0 && c == 0)      // protocol assignments
        || (a == 192 && b == 0 && c == 2)      // documentation
        || (a == 192 && b == 88 && c == 99)    // 6to4 relay anycast
        || (a == 192 && b == 168)              // private
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || (a == 198 && b == 51 && c == 100)   // documentation
        || (a == 203 && b == 0 && c == 113)    // documentation
        || a >= 224) // multicast, reserved, broadcast
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let [s0, s1, s2, s3, s4, s5, ..] = ip.segments();
    let [_, _, p, q, r, t, .., w, x, y, z] = ip.octets();
    // An address that carries an IPv4 one is as public as that one.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    if s0 == 0x0064 && s1 == 0xff9b {
        // NAT64 64:ff9b::/96; its local-use 64:ff9b:1::/48 is refused
        return [s2, s3, s4, s5] == [0; 4] && public_v4(Ipv4Addr::new(w, x, y, z));
    }
    if s0 == 0x2002 {
        // 6to4 carries the IPv4 address in its second and third segments.
        return public_v4(Ipv4Addr::new(p, q, r, t));
    }
    // Global unicast is 2000::/3; within it, Teredo, ORCHID, benchmarking and documentation aren't.
    (s0 & 0xe000) == 0x2000
        && !(s0 == 0x2001 && s1 < 0x0200) // IETF protocol assignments, Teredo, benchmarking, ORCHID
        && !(s0 == 0x2001 && s1 == 0x0db8) // documentation
        && s0 != 0x3fff // documentation (3fff::/20)
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub(super) use resolver::PublicOnly;

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod resolver {
    use super::public_address;
    use std::net::{SocketAddr, ToSocketAddrs};

    type BoxError = Box<dyn std::error::Error + Send + Sync>;

    fn refused(message: &'static str) -> BoxError {
        Box::new(std::io::Error::other(message))
    }

    /// A resolver that answers only when every address a name has is public: a name with one
    /// private record among public ones is refused whole, so a rebinding answer can't pick it.
    pub struct PublicOnly;

    impl reqwest::dns::Resolve for PublicOnly {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let host = name.as_str().to_owned();
            let (tx, rx) = futures::channel::oneshot::channel();
            // The system resolver blocks; it runs on its own thread so no runtime is assumed.
            let spawned = std::thread::Builder::new().name("nodecraft-dns".into()).spawn(move || {
                let _ = tx.send((host.as_str(), 0u16).to_socket_addrs().map(Iterator::collect::<Vec<SocketAddr>>));
            });
            Box::pin(async move {
                spawned.map_err(|_| refused("the name lookup could not start"))?;
                let addrs = rx.await.map_err(|_| refused("the name lookup stopped"))?.map_err(|_| refused("the name didn't resolve"))?;
                if addrs.is_empty() || !addrs.iter().all(|a| public_address(a.ip())) {
                    return Err(refused("the name resolves to an address that isn't public"));
                }
                Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
            })
        }
    }
}
