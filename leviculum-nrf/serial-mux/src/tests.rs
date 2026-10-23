extern crate std;

use std::string::String;
use std::vec::Vec;

use super::*;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};

#[derive(Debug, Default, PartialEq, Eq)]
struct Split {
    text: Vec<u8>,
    frames: Vec<Vec<u8>>,
    /// Every event in order, text as `T:` and frames as `F:` with their length.
    order: Vec<String>,
}

fn split_in_reads<const N: usize>(splitter: &mut Splitter<N>, reads: &[&[u8]]) -> Split {
    let mut out = Split::default();
    for read in reads {
        splitter.feed(read, |event| match event {
            Event::Text(text) => {
                assert!(!text.is_empty(), "an empty text event carries nothing");
                let newlines = text.iter().filter(|&&b| b == b'\n').count();
                assert!(
                    newlines == 0 || (newlines == 1 && text.last() == Some(&b'\n')),
                    "one line per text event: {text:?}"
                );
                out.text.extend_from_slice(text);
                out.order
                    .push(std::format!("T:{}", String::from_utf8_lossy(text)));
            }
            Event::Frame(wire) => {
                out.order.push(std::format!("F:{}", wire.len()));
                out.frames.push(wire.to_vec());
            }
        });
    }
    out
}

fn split(stream: &[u8]) -> Split {
    split_in_reads(&mut Splitter::<MAX_FRAME_WIRE>::new(), &[stream])
}

fn escaped(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    escape_text(text, &mut |piece| out.extend_from_slice(piece));
    out
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    frame(payload, &mut wire);
    wire
}

/// A payload that needs escaping and is no text line: what a packet looks like.
fn packet(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 37 + 0x7D) as u8).collect()
}

#[test]
fn escaping_renders_the_specials_and_the_flow_bytes() {
    assert_eq!(escaped(b"plain [LORA] t=12\n"), b"plain [LORA] t=12\n");
    assert_eq!(escaped(b"a~b}c\n"), b"a\\x7eb\\x7dc\n");
    assert_eq!(escaped(&[FLAG, FLAG, ESCAPE]), b"\\x7e\\x7e\\x7d");
    assert_eq!(escaped(&[b'a', XON, b'b', XOFF]), b"a\\x11b\\x13");
    assert_eq!(escaped(b""), b"");
    for text in [
        &b"a~b}c\n"[..],
        b"~~",
        b"no specials",
        b"}",
        &[XON, XOFF, FLAG],
    ] {
        assert_eq!(escaped_len(text), escaped(text).len(), "{text:?}");
        for special in [FLAG, ESCAPE, XON, XOFF] {
            assert!(!escaped(text).contains(&special), "{text:?}");
        }
    }
}

fn flow_escaped(wire: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    escape_flow(wire, &mut |piece| out.extend_from_slice(piece));
    out
}

fn stripped(input: &[u8]) -> (Vec<u8>, Option<Flow>) {
    let mut out = Vec::new();
    let flow = strip_flow(input, &mut |piece| out.extend_from_slice(piece));
    (out, flow)
}

#[test]
fn a_flow_escaped_frame_has_no_raw_flow_byte_and_deframes_unchanged() {
    let payload: Vec<u8> = (0..=255u8).chain([XON, XOFF, FLAG, ESCAPE]).collect();
    let wire = flow_escaped(&framed(&payload));
    assert!(!wire.contains(&XON) && !wire.contains(&XOFF));
    let mut deframer = Deframer::new();
    let results = deframer.process(&wire);
    assert!(
        matches!(&results[..], [DeframeResult::Frame(data)] if *data == payload),
        "{results:?}"
    );
}

#[test]
fn flow_bytes_the_board_inserts_anywhere_are_removed_and_the_last_one_wins() {
    let wire = flow_escaped(&framed(&packet(40)));
    let mut line = wire[..10].to_vec();
    line.extend([XOFF, XON, XOFF]);
    line.extend_from_slice(&wire[10..]);
    assert_eq!(stripped(&line), (wire.clone(), Some(Flow::Stop)));
    assert_eq!(
        stripped(&[XOFF, b'a', XON]),
        (b"a".to_vec(), Some(Flow::Go))
    );
    assert_eq!(stripped(&wire), (wire.clone(), None));
}

#[test]
fn escaping_streams_runs_without_copying_them() {
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    escape_text(b"ab~cd", &mut |piece| pieces.push(piece.to_vec()));
    assert_eq!(pieces, [&b"ab"[..], b"\\x7e", b"cd"]);
}

#[test]
fn text_and_frames_interleaved_come_apart_in_order() {
    let one = framed(&packet(40));
    let two = framed(&packet(7));
    let mut stream = Vec::new();
    stream.extend_from_slice(b"[BOOT] up t=1\n");
    stream.extend_from_slice(&one);
    stream.extend_from_slice(b"[LORA] rx t=2\n");
    stream.extend_from_slice(&two);
    stream.extend_from_slice(&two);
    let got = split(&stream);
    assert_eq!(got.text, b"[BOOT] up t=1\n[LORA] rx t=2\n");
    assert_eq!(got.frames, [one.clone(), two.clone(), two.clone()]);
    assert_eq!(
        got.order,
        [
            "T:[BOOT] up t=1\n".into(),
            std::format!("F:{}", one.len()),
            "T:[LORA] rx t=2\n".into(),
            std::format!("F:{}", two.len()),
            std::format!("F:{}", two.len()),
        ]
    );
}

#[test]
fn frames_pass_verbatim_and_decode_as_the_board_sent_them() {
    let payload = packet(SERIAL_HW_MTU);
    let wire = framed(&payload);
    let got = split(&wire);
    assert_eq!(got.frames, [wire]);
    let decoded = Deframer::new().process(&got.frames[0]);
    assert_eq!(decoded, [DeframeResult::Frame(payload)]);
}

#[test]
fn a_stream_split_at_every_byte_gives_the_same_answer() {
    let mut stream = Vec::new();
    stream.extend_from_slice(b"line one\n");
    stream.extend_from_slice(&framed(&packet(64)));
    stream.extend_from_slice(&escaped(b"tilde ~ and brace }\n"));
    stream.extend_from_slice(&framed(&packet(3)));
    let whole = split(&stream);
    for cut in 0..=stream.len() {
        let (a, b) = stream.split_at(cut);
        let pieces = split_in_reads(&mut Splitter::<MAX_FRAME_WIRE>::new(), &[a, b]);
        assert_eq!(pieces.text, whole.text, "cut at {cut}");
        assert_eq!(pieces.frames, whole.frames, "cut at {cut}");
    }
    let bytes: Vec<&[u8]> = stream.chunks(1).collect();
    let single = split_in_reads(&mut Splitter::<MAX_FRAME_WIRE>::new(), &bytes);
    assert_eq!(single.text, whole.text);
    assert_eq!(single.frames, whole.frames);
}

#[test]
fn escaped_text_is_never_taken_for_a_frame() {
    let line = b"[NAME] set to ~node{x}~\n";
    let mut stream = escaped(line);
    stream.extend_from_slice(&framed(&packet(10)));
    let got = split(&stream);
    assert_eq!(got.text, escaped(line));
    assert_eq!(got.frames.len(), 1);
    assert!(Deframer::new().process(&got.text).is_empty());
}

#[test]
fn back_to_back_flags_open_rather_than_close() {
    let wire = framed(&packet(12));
    let mut stream = std::vec![FLAG, FLAG];
    stream.extend_from_slice(&wire);
    let got = split(&stream);
    assert_eq!(got.frames, [wire]);
    assert!(got.text.is_empty());
}

#[test]
fn a_lost_opening_flag_resyncs_on_the_next_text_line() {
    let lost = &framed(&packet(30))[1..];
    let next = framed(&packet(9));
    let mut stream = lost.to_vec();
    stream.extend_from_slice(b"[LORA] tx t=5\n");
    stream.extend_from_slice(b"[LORA] rx t=6\n");
    stream.extend_from_slice(&next);
    stream.extend_from_slice(b"[LORA] tx t=7\n");
    let got = split(&stream);
    assert_eq!(got.frames, [next], "the frame after the tear is whole");
    assert!(got
        .text
        .ends_with(b"[LORA] tx t=5\n[LORA] rx t=6\n[LORA] tx t=7\n"));
}

#[test]
fn a_lost_opening_flag_resyncs_on_back_to_back_frames() {
    let lost = &framed(&packet(30))[1..];
    let next = framed(&packet(9));
    let mut stream = lost.to_vec();
    stream.extend_from_slice(&next);
    stream.extend_from_slice(b"after\n");
    let got = split(&stream);
    assert_eq!(got.frames, [next]);
    assert!(got.text.ends_with(b"after\n"));
}

#[test]
fn garbage_before_the_first_frame_is_text_and_the_frame_survives() {
    let garbage: Vec<u8> = (0u8..=255).filter(|&b| b != FLAG).collect();
    let wire = framed(&packet(20));
    let mut stream = garbage.clone();
    stream.extend_from_slice(&wire);
    let got = split(&stream);
    assert_eq!(got.text, garbage);
    assert_eq!(got.frames, [wire]);
}

#[test]
fn a_frame_at_the_bound_passes_and_one_past_it_falls_back_to_text() {
    const N: usize = 16;
    let fits: Vec<u8> = framed(&[0xA4; N - 2]);
    assert_eq!(fits.len(), N);
    let got = split_in_reads(&mut Splitter::<N>::new(), &[&fits]);
    assert_eq!(got.frames, [fits]);

    let mut splitter = Splitter::<N>::new();
    let mut stream = framed(&[0xA4; N - 1]);
    stream.extend_from_slice(&framed(&[0xA5; 4]));
    let got = split_in_reads(&mut splitter, &[&stream]);
    assert_eq!(
        got.text,
        [0xA4; N - 1],
        "the overlong run is handed on as text"
    );
    // Its closing flag opened a frame that the next opener keeps open.
    assert_eq!(got.frames, [framed(&[0xA5; 4])]);
    assert!(!splitter.in_frame());
}

#[test]
fn the_largest_leviculum_frame_fits_the_default_bound() {
    let wire = framed(&[FLAG; SERIAL_HW_MTU]);
    assert_eq!(wire.len(), MAX_FRAME_WIRE);
    assert_eq!(split(&wire).frames, [wire]);
}

#[test]
fn a_partial_line_continues_in_the_next_read() {
    let got = split_in_reads(
        &mut Splitter::<MAX_FRAME_WIRE>::new(),
        &[b"[FW_BUILD] git_s", b"ha=abc t=9\n", b"next"],
    );
    assert_eq!(
        got.order,
        ["T:[FW_BUILD] git_s", "T:ha=abc t=9\n", "T:next"]
    );
}

#[test]
fn reset_drops_a_half_read_frame() {
    let mut splitter = Splitter::<MAX_FRAME_WIRE>::new();
    let wire = framed(&packet(8));
    let got = split_in_reads(&mut splitter, &[&wire[..4]]);
    assert!(got.frames.is_empty() && got.text.is_empty());
    assert!(splitter.in_frame());
    splitter.reset();
    let got = split_in_reads(&mut splitter, &[b"fresh\n"]);
    assert_eq!(got.text, b"fresh\n");
}
