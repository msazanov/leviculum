//! Python Channel send with receipt polling, for the per-link cipher interop tests.
use super::*;

impl TestDaemon {
    /// Send a MessageBase payload through Python RNS Channel and poll its packet
    /// proof/ACK state. `tries == 1` plus DELIVERED demonstrates the initial
    /// Channel packet was proven without retransmission (Python counts the
    /// initial send as attempt one).
    pub async fn send_channel_on_link(
        &self,
        link_hash: &str,
        data: &[u8],
        timeout: Duration,
    ) -> Result<(u16, String, bool, u8), HarnessError> {
        let sent = self
            .query(
                "send_channel_message",
                serde_json::json!({
                    "link_hash": link_hash,
                    "data": hex::encode(data),
                }),
            )
            .await?;
        let sequence = sent
            .get("sequence")
            .and_then(|value| value.as_u64())
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| HarnessError::ParseError("missing Channel sequence".into()))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self
                .query(
                    "get_channel_send_state",
                    serde_json::json!({"link_hash": link_hash, "sequence": sequence}),
                )
                .await?;
            let state = status
                .get("state")
                .and_then(|value| value.as_str())
                .ok_or_else(|| HarnessError::ParseError("missing Channel receipt state".into()))?
                .to_string();
            let pending = status
                .get("pending")
                .and_then(|value| value.as_bool())
                .ok_or_else(|| HarnessError::ParseError("missing Channel pending flag".into()))?;
            let tries = status
                .get("tries")
                .and_then(|value| value.as_u64())
                .and_then(|value| u8::try_from(value).ok())
                .ok_or_else(|| HarnessError::ParseError("missing Channel retry count".into()))?;
            if state != "MSGSTATE_SENT" || tokio::time::Instant::now() >= deadline {
                return Ok((sequence, state, pending, tries));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
