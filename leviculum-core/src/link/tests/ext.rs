//! Tests for GCM mode 7 Links and their LRRTT retry interval.
use super::*;

#[test]
fn gcm_rtt_retry_interval_is_three_rtts_with_ten_second_floor() {
    let dest_hash = DestinationHash::new([0x4A; TRUNCATED_HASHBYTES]);
    let mut link = Link::new_outgoing(dest_hash, &mut OsRng);
    link.set_rtt_ms(3_000);
    assert_eq!(link.rtt_retry_interval_ms(), RTT_RETRY_MIN_INTERVAL_MS);
    link.set_rtt_ms(4_000);
    assert_eq!(
        link.rtt_retry_interval_ms(),
        4_000 * RTT_RETRY_INTERVAL_MULTIPLIER
    );
}

#[test]
fn gcm_handshake_rtt_and_bidirectional_data() {
    let identity = Identity::generate(&mut OsRng);
    let dest = DestinationHash::new([42; 16]);
    let mut a = Link::new_outgoing(dest, &mut OsRng);
    a.set_cipher(LinkCipher::Gcm).unwrap();
    let request = a.build_link_request_packet_with_transport(None, 1, Some(16384));
    let mut b =
        Link::new_incoming(&request[19..], *a.id(), dest, &mut OsRng, Some(16384)).unwrap();
    assert_eq!(b.cipher(), LinkCipher::Gcm);
    let proof = b
        .build_proof_packet(&identity, 16384, LinkCipher::Gcm as u8)
        .unwrap();
    a.set_destination_keys(identity.ed25519_verifying().as_bytes())
        .unwrap();
    let mut corrupted = proof.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0x20;
    assert!(a.process_proof(&corrupted[19..]).is_err());
    a.process_proof(&proof[19..]).unwrap();
    let rtt = a.build_rtt_packet(0.001, &mut OsRng).unwrap();
    b.process_rtt(&rtt[19..], 1_000).unwrap();
    let mut token = alloc::vec![0; 16100];
    let mut out = alloc::vec![0; 16100];
    let n = a.encrypt(&[7; 16000], &mut token, &mut OsRng).unwrap();
    assert_eq!(n, 16028);
    assert_eq!(b.decrypt(&token[..n], &mut out).unwrap(), 16000);
    assert_eq!(&out[..16000], &[7; 16000]);
    assert!(a.decrypt(&token[..n], &mut out).is_err());
    let n = b.encrypt(b"reply", &mut token, &mut OsRng).unwrap();
    assert_eq!(a.decrypt(&token[..n], &mut out).unwrap(), 5);
    assert_eq!(&out[..5], b"reply");
    assert!(a.set_cipher(LinkCipher::Cbc).is_err());
}

#[test]
fn signed_cbc_response_cannot_downgrade_a_gcm_request() {
    let identity = Identity::generate(&mut OsRng);
    let dest = DestinationHash::new([42; 16]);
    let mut a = Link::new_outgoing(dest, &mut OsRng);
    a.set_cipher(LinkCipher::Gcm).unwrap();
    let mut request = a.build_link_request_packet_with_transport(None, 1, Some(16384));
    let offset = request.len() - SIGNALING_SIZE;
    request[offset..].copy_from_slice(&encode_signaling_bytes(16384, LinkCipher::Cbc as u8));
    let mut b =
        Link::new_incoming(&request[19..], *a.id(), dest, &mut OsRng, Some(16384)).unwrap();
    let proof = b
        .build_proof_packet(&identity, 16384, LinkCipher::Cbc as u8)
        .unwrap();
    a.set_destination_keys(identity.ed25519_verifying().as_bytes())
        .unwrap();
    assert_eq!(
        a.process_proof(&proof[19..]),
        Err(LinkError::UnsupportedMode)
    );
    assert!(a.ext.gcm.is_none());
}

#[test]
fn gcm_refuses_a_noncontributory_peer_key() {
    let identity = Identity::generate(&mut OsRng);
    let dest = DestinationHash::new([42; 16]);
    let a = Link::new_outgoing(dest, &mut OsRng);
    let mut request = a.create_link_request_with_mtu(16384, LinkCipher::Gcm as u8);
    request[..32].fill(0);
    let mut b = Link::new_incoming(
        &request,
        LinkId::new([1; 16]),
        dest,
        &mut OsRng,
        Some(16384),
    )
    .unwrap();
    assert_eq!(
        b.build_proof_packet(&identity, 16384, LinkCipher::Gcm as u8),
        Err(LinkError::KeyExchangeFailed)
    );
}
