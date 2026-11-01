use super::*;

#[test]
fn send_window_ceiling_survives_rtt_updates_and_success() {
    let mut channel = Channel::new();
    channel.set_window_ceiling(4);
    for _ in 0..100 {
        channel.adjust_window(true, 1);
        assert!(channel.window <= 4);
        assert!(channel.window_max <= 4);
    }
    channel.set_window_ceiling(16);
    channel.update_window_for_rtt(1);
    assert_eq!(channel.window_max, 16);
}

#[test]
fn window_only_keeps_backpressure_and_retry_budget() {
    let mut channel = Channel::new();
    channel.update_window_for_rtt(100);
    channel.set_window_ceiling(4);
    channel.set_pacing_enabled(false);
    channel.adjust_window(true, 100);
    assert_eq!(channel.pacing_interval_ms(), 0);
    let message = TestMessage {
        data: alloc::vec![1],
    };
    for _ in 0..channel.window {
        channel.send(&message, 500, 0, 100).unwrap();
    }
    assert_eq!(channel.send(&message, 500, 0, 100), Err(ChannelError::Busy));
    channel.poll(100000, 100);
    assert_eq!(channel.pacing_interval_ms(), 0);
    assert_eq!(crate::constants::CHANNEL_MAX_TRIES, 8);
    channel.set_pacing_enabled(true);
    channel.adjust_window(true, 100);
    assert!(channel.pacing_interval_ms() > 0);
}
