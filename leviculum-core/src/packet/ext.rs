//! Packet classification used by per-link ciphers: the token tag a data proof can bind to,
//! and the link data class that skips the full-payload hash.
use super::*;

/// HMAC tag of an encrypted packet's token, i.e. its last 32 data bytes.
///
/// Link data packets carry `IV(16) | ciphertext | HMAC-SHA256(32)` in the data
/// field, and the tag is produced as a by-product of the encrypt/verify step
/// that happens anyway. Handing it to the receipt/proof machinery lets a data
/// proof bind to a value nobody had to hash a second time.
///
/// Returns `None` when the packet cannot be a token packet: too short, no
/// usable header, or a context whose data is sent in cleartext (link control,
/// keepalive, path responses) — for those the trailing bytes are payload and
/// must not be mistaken for a tag.
pub fn packet_token_tag(raw: &[u8]) -> Option<[u8; 32]> {
    if raw.len() < HEADER_MINSIZE {
        return None;
    }
    let flags = raw[0];
    let is_header_type_2 = (flags & FLAG_HEADER_TYPE_MASK) != 0;
    let data_start = if is_header_type_2 {
        2 + TRUNCATED_HASHBYTES
    } else {
        2
    };
    let header_end = data_start + TRUNCATED_HASHBYTES + 1; // dest_hash + context
    if raw.len() < header_end {
        return None;
    }
    token_tag(
        PacketContext::from_byte(raw[data_start + TRUNCATED_HASHBYTES]),
        &raw[header_end..],
    )
}

/// [`packet_token_tag`] for a packet already split into context and data.
pub fn token_tag(context: PacketContext, data: &[u8]) -> Option<[u8; 32]> {
    use crate::constants::{AES_BLOCK_SIZE, HMAC_SIZE};
    // Smallest possible token: IV + one AES block + tag.
    const MIN_TOKEN_SIZE: usize = AES_BLOCK_SIZE + AES_BLOCK_SIZE + HMAC_SIZE;
    // Only provable contexts are token-encrypted; see the link builder.
    match context {
        PacketContext::Resource
        | PacketContext::ResourceAdv
        | PacketContext::ResourceHmu
        | PacketContext::ResourceRcl
        | PacketContext::Channel => {}
        _ => return None,
    }
    if data.len() < MIN_TOKEN_SIZE {
        return None;
    }
    let tag = &data[data.len() - HMAC_SIZE..];
    let mut out = [0u8; HMAC_SIZE];
    out.copy_from_slice(tag);
    Some(out)
}

/// Link Channel data: the hot-path packet class whose full-payload hash nothing reads.
///
/// SHA-256 over every received payload byte was the largest single cost of a
/// bulk link on a one-core node (73% of the receive thread in the profile of
/// `docs/benchmarks-1core.md`). For this class every consumer of the hash turns
/// out to be dead weight:
///
/// * the duplicate check is skipped (`Channel` is a dedup-exempt context: its
///   retransmissions are byte-identical and the handlers are idempotent), and
///   for the same reason inserting the hash into the table is inert;
/// * delivery proofs bind to the token tag / nonce+tag of the link layer, not to
///   this hash (`cipher_proof_identifier`);
/// * a Channel proof is addressed to the link id and routed by the link table,
///   never through the reverse table keyed by the packet hash;
/// * the node's "repack symmetry" check only logs a warning.
///
/// Authenticity and replay protection are untouched: the link layer verifies the
/// AEAD tag and the GCM replay window per record, whatever the transport did.
/// The class is deliberately narrow (Data + Link destination + Channel context)
/// so that everything else keeps hashing exactly as before.
pub fn is_hash_free_link_data(flags: &PacketFlags, context: PacketContext) -> bool {
    flags.packet_type == PacketType::Data
        && flags.dest_type == DestinationType::Link
        && matches!(context, PacketContext::Channel | PacketContext::Tunnel)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only Data + Link destination + Channel context may skip the payload hash;
    /// every neighbouring class must keep hashing.
    #[test]
    fn hash_free_class_is_exactly_link_channel_data() {
        let flags = |packet_type, dest_type| PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type,
            packet_type,
        };
        let channel = PacketContext::Channel;
        assert!(is_hash_free_link_data(&flags(PacketType::Data, DestinationType::Link), channel));
        assert!(is_hash_free_link_data(
            &flags(PacketType::Data, DestinationType::Link),
            PacketContext::Tunnel
        ));
        assert_eq!(PacketContext::from_byte(0xF0), PacketContext::Tunnel);
        assert_eq!(PacketContext::Tunnel.to_byte(), 0xF0);
        for context in [
            PacketContext::None,
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
            assert!(!is_hash_free_link_data(&flags(packet_type, DestinationType::Link), channel));
        }
        for dest_type in [DestinationType::Single, DestinationType::Group, DestinationType::Plain] {
            assert!(!is_hash_free_link_data(&flags(PacketType::Data, dest_type), channel));
        }
    }

    /// The token tag is what data proofs now bind to, so its boundaries matter:
    /// a cleartext packet must never have trailing payload read as a tag, and a
    /// real token packet must yield exactly its HMAC bytes.
    #[test]
    fn token_tag_is_only_read_from_token_packets() {
        use crate::constants::{AES_BLOCK_SIZE, HMAC_SIZE};
        let min_token = AES_BLOCK_SIZE + AES_BLOCK_SIZE + HMAC_SIZE;

        // Type1 header (flags, hops, dest_hash, context) + token-shaped data.
        let mut raw = alloc::vec::Vec::new();
        raw.extend_from_slice(&[0x00, 0x00]);
        raw.extend_from_slice(&[0x11u8; 16]);
        raw.push(PacketContext::Channel.to_byte());
        raw.extend_from_slice(&[0u8; AES_BLOCK_SIZE]); // IV
        raw.extend_from_slice(&[0u8; AES_BLOCK_SIZE]); // ciphertext
        let tag: [u8; 32] = core::array::from_fn(|i| 0xA0 + i as u8);
        raw.extend_from_slice(&tag);
        assert_eq!(packet_token_tag(&raw), Some(tag));

        // Same shape, but a context that is never token-encrypted: the trailing
        // bytes are payload and must not be taken for a tag.
        let mut cleartext = raw[..raw.len() - HMAC_SIZE].to_vec();
        cleartext.push(PacketContext::LinkClose.to_byte());
        let ctx_at = 2 + 16;
        cleartext[ctx_at] = PacketContext::LinkClose.to_byte();
        assert_eq!(packet_token_tag(&cleartext), None);

        // Truncated: below HEADER_MINSIZE, and data shorter than a token.
        assert_eq!(packet_token_tag(&[0x00, 0x00]), None);
        let mut tiny = raw[..raw.len() - HMAC_SIZE].to_vec();
        tiny[ctx_at] = PacketContext::Channel.to_byte();
        assert_eq!(packet_token_tag(&tiny), None);
        let _ = min_token;
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
