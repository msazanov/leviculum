#![no_main]
//! Fuzz the micron markup parser (`leviculum_micron::parse`) with arbitrary
//! bytes, decoded the way lnomad decodes a fetched page
//! (`String::from_utf8_lossy`, `lnomad/src/browser.rs`). The page comes from a
//! remote node, so a panic here is a remote crash of the browser.
//!
//! Beyond "never panics", two properties the renderer relies on are asserted
//! on every document:
//!
//! - no string in the model carries a control character or a C1 code point,
//!   the guarantee `strip_control_chars` gives against terminal escape
//!   injection (Codeberg #281, `tests/parser_tests.rs::assert_escape_free`);
//! - every block index in `anchors` and `header_rows` names a block that
//!   exists, because lnomad looks blocks up by those indices.
use leviculum_micron::{parse, Block, Line, MicronDocument};
use libfuzzer_sys::fuzz_target;

fn hostile(c: char) -> bool {
    c.is_control() || ('\u{80}'..='\u{9f}').contains(&c)
}

fn check_str(s: &str) {
    if let Some(c) = s.chars().find(|&c| hostile(c)) {
        panic!("control char U+{:04X} survived in {s:?}", c as u32);
    }
}

fn check_line(line: &Line) {
    for span in &line.spans {
        check_str(&span.text);
        if let Some(link) = &span.link {
            check_str(&link.label);
            check_str(&link.target);
            link.fields.iter().for_each(|f| check_str(f));
        }
        if let Some(field) = &span.field {
            check_str(&field.name);
            check_str(&field.value);
            check_str(&field.label);
        }
        if let Some(anchor) = &span.anchor {
            check_str(anchor);
        }
    }
}

fn check(doc: &MicronDocument) {
    for block in &doc.blocks {
        match block {
            Block::Heading { line, .. } | Block::Paragraph { line, .. } => check_line(line),
            Block::LiteralBlock { lines } => lines.iter().for_each(check_line),
            Block::Table { rows, .. } => rows.iter().for_each(|r| check_str(r)),
            Block::Partial { url, fields, .. } => {
                check_str(url);
                fields.iter().for_each(|f| check_str(f));
            }
            Block::Divider { character, .. } => assert!(!hostile(*character)),
            Block::Blank => {}
        }
    }
    for (name, &idx) in &doc.anchors {
        check_str(name);
        assert!(
            idx < doc.blocks.len(),
            "anchor {name:?} -> {idx} past the end"
        );
    }
    for &idx in &doc.header_rows {
        assert!(idx < doc.blocks.len(), "header row {idx} past the end");
    }
}

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    check(&parse(&source));
});
