//! Order 1055 (#469): no board binary may spin on `nop` in its boot path.
//!
//! The T114's boot blink was `for _ in 0..12_000_000u32 { nop() }` in
//! `main`, before the main loop. It held the executor, and with it the USB
//! task, which had not run yet: 2093 of the 3317 ms until `usb_up` on
//! d5981450 (`[BOOT_TRACE] stage=blink_done t=2599` after `post-init`
//! at 506). The blink now runs on a timer in the LED player
//! (`feedback::spawn_led_with_boot_blink`). This pins that no `bin/*.rs`
//! grows such a spin again: a `for` loop over a literal range above
//! [`MAX_SPIN`] whose body is a `nop`.
//!
//! The fault handler's blink in `lib.rs` spins on purpose (it owns the
//! core until the reset) and is out of scope, as is any bounded short
//! spin of a driver.
//!
//! Lives in leviculum-std because leviculum-nrf cross-compiles to
//! thumbv7em and cannot run host tests, as `rak4631_gnss_pulse_pin.rs`.

use std::path::{Path, PathBuf};

/// The largest `nop` loop count a binary may run inline: at 64 MHz a few
/// cycles an iteration, under a millisecond.
const MAX_SPIN: u64 = 10_000;

/// Every `for _ in 0..N` whose body (to its closing brace) calls `nop`
/// with `N > MAX_SPIN`, as `(line, N)`.
fn long_nop_spins(src: &str) -> Vec<(usize, u64)> {
    let mut found = Vec::new();
    let needle = "for _ in 0..";
    let mut from = 0;
    while let Some(off) = src[from..].find(needle) {
        let start = from + off;
        let rest = &src[start + needle.len()..];
        let digits: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '_')
            .filter(|c| *c != '_')
            .collect();
        from = start + needle.len();
        let Ok(count) = digits.parse::<u64>() else {
            continue;
        };
        let Some(open) = rest.find('{') else {
            continue;
        };
        let mut depth = 0usize;
        let mut body_end = rest.len();
        for (i, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        body_end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if count > MAX_SPIN && rest[open..body_end].contains("nop()") {
            found.push((src[..start].matches('\n').count() + 1, count));
        }
    }
    found
}

#[test]
fn no_board_binary_spins_on_nop_before_the_main_loop() {
    let bin: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("leviculum-nrf/src/bin");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&bin)
        .unwrap_or_else(|e| panic!("read {}: {e}", bin.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    // The four boards at least: an empty or misplaced directory must not
    // pass by reading nothing.
    for board in ["t114.rs", "t1000e.rs", "solarnode.rs", "rak4631.rs"] {
        assert!(
            files.iter().any(|p| p.ends_with(board)),
            "{} has no {board}",
            bin.display()
        );
    }
    let mut offenders = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (line, count) in long_nop_spins(&src) {
            offenders.push(format!("{}:{line} spins {count} nops", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "a nop spin above {MAX_SPIN} holds the executor (and USB) at boot; \
         put it on a Timer (order 1055):\n{}",
        offenders.join("\n")
    );
}

/// Positive control: the loop this replaced trips the detector, and a
/// short spin or a loop without `nop` does not.
#[test]
fn the_old_boot_blink_trips_the_detector() {
    let old = "    led.set_level(Level::Low);\n    for _ in 0..12_000_000u32 {\n        cortex_m::asm::nop();\n    }\n    led.set_level(Level::High);\n";
    assert_eq!(long_nop_spins(old), vec![(2, 12_000_000)]);
    let short = "for _ in 0..1_000u32 {\n    cortex_m::asm::nop();\n}\n";
    assert!(long_nop_spins(short).is_empty());
    let no_nop = "for _ in 0..12_000_000u32 {\n    x += 1;\n}\n";
    assert!(long_nop_spins(no_nop).is_empty());
}
