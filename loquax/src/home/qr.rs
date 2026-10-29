//! The admission code as a QR code in the terminal, for a phone to take in
//! one scan: Unicode half blocks, two module rows per text row, with the
//! standard quiet zone of four modules around it.
//!
//! The code's base32 goes in upper case in the QR alphanumeric mode, 5.5
//! bits a character instead of 8 (the code reads in either case), and the
//! address behind it in byte mode. Light modules are drawn, dark ones left
//! blank, as `qrencode -t UTF8` does: right on a terminal with light text on
//! a dark ground, which is most of them; scanners read the inverse too.

use alloc::string::String;
use alloc::vec::Vec;

use qrcodegen::{QrCode, QrCodeEcc, QrSegment};

use super::code::SERVER_SEPARATOR;

/// The light margin a scanner needs around the code, in modules.
pub const QUIET: i32 = 4;

/// Encode `text` (a code from [`super::code::HomeCode`], with or without an
/// address) for the screen.
pub fn encode(text: &str) -> Result<QrCode, String> {
    let (code, server) = match text.split_once(SERVER_SEPARATOR) {
        Some((code, server)) => (code, Some(server)),
        None => (text, None),
    };
    let code = code.to_ascii_uppercase();
    let mut segments = Vec::with_capacity(2);
    segments.push(match QrSegment::is_alphanumeric(&code) {
        true => QrSegment::make_alphanumeric(&code),
        false => QrSegment::make_bytes(code.as_bytes()),
    });
    if let Some(server) = server {
        let mut tail = String::new();
        tail.push(SERVER_SEPARATOR);
        tail.push_str(server);
        segments.push(QrSegment::make_bytes(tail.as_bytes()));
    }
    QrCode::encode_segments(&segments, QrCodeEcc::Medium)
        .map_err(|error| alloc::format!("the code does not fit a QR code: {error}"))
}

/// The QR code of `text` as lines of half blocks.
pub fn lines(text: &str) -> Result<Vec<String>, String> {
    Ok(draw(&encode(text)?))
}

/// One QR code as lines of half blocks.
pub fn draw(qr: &QrCode) -> Vec<String> {
    // Outside the symbol is quiet zone, and quiet is light.
    let light = |x: i32, y: i32| !qr.get_module(x, y);
    let size = qr.size();
    let mut out = Vec::new();
    let mut y = -QUIET;
    while y < size + QUIET {
        let mut line = String::new();
        for x in -QUIET..size + QUIET {
            let below = y + 1 < size + QUIET && light(x, y + 1);
            line.push(match (light(x, y), below) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push(line);
        y += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::code::HomeCode;

    fn code() -> HomeCode {
        HomeCode {
            public_key: [0x3c; 64],
            token: [0x99; 16],
        }
    }

    /// Read the modules back off the drawing: light where the half block
    /// is drawn.
    fn modules(lines: &[String], width: i32) -> Vec<Vec<bool>> {
        let mut rows = Vec::new();
        for line in lines {
            let (mut top, mut bottom) = (Vec::new(), Vec::new());
            for c in line.chars() {
                top.push(matches!(c, '█' | '▀'));
                bottom.push(matches!(c, '█' | '▄'));
            }
            assert_eq!(top.len() as i32, width);
            rows.push(top);
            rows.push(bottom);
        }
        rows
    }

    fn check(text: &str) -> QrCode {
        let qr = encode(text).expect("it fits");
        let lines = draw(&qr);
        let width = qr.size() + 2 * QUIET;
        assert_eq!(lines.len() as i32, (width + 1) / 2);
        let read = modules(&lines, width);
        for y in -QUIET..qr.size() + QUIET {
            for x in -QUIET..qr.size() + QUIET {
                let drawn = read[(y + QUIET) as usize][(x + QUIET) as usize];
                assert_eq!(drawn, !qr.get_module(x, y), "module {x},{y}");
            }
        }
        qr
    }

    #[test]
    fn the_drawing_is_the_symbol_with_its_quiet_zone() {
        let qr = check(&code().encode());
        // 140 alphanumeric characters at level M: version 6, 41 modules.
        assert_eq!(qr.version().value(), 6);
        let qr = check(&code().encode_with_server("homenode.example.org:4242"));
        assert!(qr.version().value() <= 9, "{}", qr.version().value());
        check(&code().encode_with_server("[fd00::1]:4242"));
    }

    #[test]
    fn the_code_goes_in_the_dense_mode() {
        // The same text in byte mode needs a larger symbol: the upper case
        // alphanumeric segment is what keeps it small.
        let text = code().encode();
        let bytes =
            QrCode::encode_segments(&[QrSegment::make_bytes(text.as_bytes())], QrCodeEcc::Medium)
                .expect("fits");
        assert!(encode(&text).expect("fits").size() < bytes.size());
    }
}
