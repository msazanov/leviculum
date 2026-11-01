use super::*;

#[test]
fn bulk_deframer_matches_scalar_across_fragments_and_limits() {
    // Arbitrary streams, including noise, escapes split across reads,
    // consecutive flags and frames exceeding the receive ceiling.
    for cap in [0, 1, 8, 600, 16384] {
        for chunk in [1, 2, 7, 1024, 17000] {
            let mut seed = 0x12345678u32;
            let mut data = Vec::new();
            for i in 0..40000 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                data.push(match i % 97 {
                    0 => FLAG,
                    1 | 2 => ESCAPE,
                    _ => seed as u8,
                });
            }
            let mut bulk = Deframer::with_max_frame(cap);
            let mut scalar = Deframer::with_max_frame(cap);
            for part in data.chunks(chunk) {
                let expected: Vec<_> = part
                    .iter()
                    .filter_map(|&b| scalar.process_byte(b))
                    .collect();
                assert_eq!(bulk.process(part), expected, "cap={cap}, chunk={chunk}");
                assert_eq!(bulk.buffer, scalar.buffer);
                assert_eq!(bulk.in_frame, scalar.in_frame);
                assert_eq!(bulk.escape_next, scalar.escape_next);
            }
        }
    }
}
