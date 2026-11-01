use alloc::vec::Vec;

use super::{
    needs_escape, DeframeResult, Deframer, ESCAPE, ESCAPE_XOR, FLAG, INITIAL_BUFFER_CAPACITY,
};

/// Copy ordinary runs in bulk; only special bytes need escaping.
pub(super) fn frame_bulk(data: &[u8], output: &mut Vec<u8>) {
    let mut rest = data;
    while let Some(pos) = memchr::memchr2(FLAG, ESCAPE, rest) {
        output.extend_from_slice(&rest[..pos]);
        output.push(ESCAPE);
        output.push(rest[pos] ^ ESCAPE_XOR);
        rest = &rest[pos + 1..];
    }
    output.extend_from_slice(rest);
}

/// Bulk deframing path using memchr to find flags and escapes without
/// running the state machine per byte over ordinary data runs.
pub(super) fn deframer_process_bulk(deframer: &mut Deframer, data: &[u8]) -> Vec<DeframeResult> {
    let mut results = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        if !deframer.in_frame {
            // Discard noise without running the state machine per byte.
            let Some(pos) = memchr::memchr(FLAG, rest) else {
                break;
            };
            deframer.process_byte(FLAG);
            rest = &rest[pos + 1..];
            continue;
        }
        if deframer.escape_next || needs_escape(rest[0]) {
            if let Some(result) = deframer.process_byte(rest[0]) {
                results.push(result);
            }
            rest = &rest[1..];
            continue;
        }
        let len = memchr::memchr2(FLAG, ESCAPE, rest).unwrap_or(rest.len());
        let room = deframer.max_frame - deframer.buffer.len();
        if len > room {
            // Match the scalar path exactly: consume the first overflowing
            // byte, discard the frame, then resynchronise at the next FLAG.
            deframer.reset();
            deframer.buffer.shrink_to(INITIAL_BUFFER_CAPACITY);
            results.push(DeframeResult::Oversized);
            rest = &rest[room + 1..];
        } else {
            deframer.buffer.extend_from_slice(&rest[..len]);
            rest = &rest[len..];
        }
    }
    results
}
