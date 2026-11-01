use super::Channel;
use crate::constants::CHANNEL_WINDOW_MAX_FAST;

impl Channel {
    /// Bound outstanding records to reduce queueing; ACK policy is unchanged.
    pub fn set_window_ceiling(&mut self, ceiling: usize) {
        self.window_ceiling = ceiling.clamp(1, CHANNEL_WINDOW_MAX_FAST);
        self.window_max = self.window_max.min(self.window_ceiling);
        self.window_min = self.window_min.min(self.window_max);
        self.window = self.window.min(self.window_max);
    }

    /// Explicit reliable-underlay policy: retain window backpressure while
    /// leaving transport pacing to QUIC. Default remains enabled for RNS.
    pub fn set_pacing_enabled(&mut self, enabled: bool) {
        self.pacing_enabled = enabled;
        if !enabled {
            self.pacing_interval_ms = 0;
            self.next_send_at_ms = 0;
        }
    }
}
