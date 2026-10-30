//! Per-link keepalive override on a live link, and the keepalive round trip the initiator times.
use super::*;

/// A per-link keepalive override applies to both ends of a live link; the
/// initiator measures the keepalive round trip. Without the override both
/// ends would use the 5 s RTT-derived floor.
#[test]
fn per_link_keepalive_override_and_keepalive_rtt() {
    let mut p = establish(0, 0);
    let l = p.initiator.link(&p.link_id).expect("link");
    assert_eq!(l.keepalive_ms(), 5_000, "precondition: RTT-derived floor");
    assert_eq!(l.keepalive_rtt_ms(), None);

    for node in [&mut p.initiator, &mut p.responder] {
        node.link_mut(&p.link_id).expect("link").set_keepalive_override(Some(25));
    }
    assert!(p.initiator.link_mut(&LinkId::new([0xAB; 16])).is_none());
    for node in [&p.initiator, &p.responder] {
        let l = node.link(&p.link_id).expect("link");
        assert_eq!(l.keepalive_ms(), 25_000);
        assert_eq!(l.stale_time_ms(), 50_000);
    }

    // No keepalive before 25 s, one at 25 s; its echo 40 ms later is timed.
    p.initiator.transport().clock().set(TEST_TIME_MS + 10_000);
    let tick = p.initiator.handle_timeout();
    assert!(with_context(&action_data(&tick), PacketContext::Keepalive).is_empty());
    let ka = initiator_keepalive(&mut p, 25_000);
    p.responder.transport().clock().set(TEST_TIME_MS + 25_020);
    let echo = deliver_all(&mut p.responder, p.r_iface, std::vec![ka]);
    let echo = with_context(&echo, PacketContext::Keepalive);
    assert_eq!(echo.len(), 1, "the responder echoes");
    p.initiator.transport().clock().set(TEST_TIME_MS + 25_040);
    deliver_all(&mut p.initiator, p.i_iface, echo);
    let l = p.initiator.link(&p.link_id).expect("link");
    assert_eq!(l.keepalives_sent(), 1);
    assert_eq!(l.keepalives_acked(), 1);
    assert_eq!(l.keepalive_rtt_ms(), Some(40));
}
