//! The link data class that skips the transport's full-payload hash.
use super::*;

/// Tunnel datagrams on a Link: the hot-path packet class whose full-payload hash nothing reads.
///
/// SHA-256 over every received payload byte was the largest single cost of a
/// bulk link on a one-core node (73% of the receive thread in the profile of
/// `docs/benchmarks-1core.md`). For a tunnel datagram every consumer of the hash
/// is dead weight:
///
/// * it is never proven, so no receipt or proof refers to it;
/// * it is not checked against the duplicate table: an IP tunnel copes with a
///   duplicate datagram, and replay is refused by the link layer below;
/// * it is addressed to the link id and routed by the link table, never through
///   the reverse table keyed by the packet hash.
///
/// Authenticity and replay protection are untouched: the link layer verifies the
/// AEAD tag and the GCM replay window per record, whatever the transport did.
/// Channel data is not in the class: its proof is the vanilla packet hash, which
/// the transport computes once and the link layer reuses.
pub fn is_hash_free_link_data(flags: &PacketFlags, context: PacketContext) -> bool {
    flags.packet_type == PacketType::Data
        && flags.dest_type == DestinationType::Link
        && context == PacketContext::Tunnel
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only Data + Link destination + Tunnel context may skip the payload hash;
    /// every neighbouring class, Channel included, must keep hashing.
    #[test]
    fn hash_free_class_is_exactly_link_tunnel_data() {
        let flags = |packet_type, dest_type| PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type,
            packet_type,
        };
        let tunnel = PacketContext::Tunnel;
        assert!(is_hash_free_link_data(&flags(PacketType::Data, DestinationType::Link), tunnel));
        assert_eq!(PacketContext::from_byte(0xF0), PacketContext::Tunnel);
        assert_eq!(PacketContext::Tunnel.to_byte(), 0xF0);
        for context in [
            PacketContext::None,
            PacketContext::Channel,
            PacketContext::Resource,
            PacketContext::ResourcePrf,
            PacketContext::Keepalive,
            PacketContext::LinkClose,
            PacketContext::Lrproof,
            PacketContext::LinkProof,
            PacketContext::Unknown(0x42),
        ] {
            assert!(!is_hash_free_link_data(&flags(PacketType::Data, DestinationType::Link), context));
        }
        for packet_type in [PacketType::Announce, PacketType::LinkRequest, PacketType::Proof] {
            assert!(!is_hash_free_link_data(&flags(packet_type, DestinationType::Link), tunnel));
        }
        for dest_type in [DestinationType::Single, DestinationType::Group, DestinationType::Plain] {
            assert!(!is_hash_free_link_data(&flags(PacketType::Data, dest_type), tunnel));
        }
    }

    /// packet_hash must stay bit-for-bit equal to hashing the materialised
    /// hashable part: the digest goes on the wire into data proofs, so a
    /// divergence here silently breaks interop with Python Reticulum.
    #[test]
    fn packet_hash_matches_materialised_hashable_part() {
        for len in [2usize, 3, 17, 18, 19, 64, 1000, 16000] {
            for header in [0x00u8, 0x40, 0x80, 0xC0] {
                let raw: alloc::vec::Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(header)).collect();
                let expected = crate::crypto::sha256(&get_hashable_part(&raw));
                assert_eq!(packet_hash(&raw), expected, "len={len} header={header:#x}");
            }
        }
        // Too short to have any hashable part: both paths hash the empty slice.
        for len in [0usize, 1] {
            let raw = alloc::vec![0u8; len];
            assert_eq!(packet_hash(&raw), crate::crypto::sha256(&get_hashable_part(&raw)), "len={len}");
        }
    }
}
