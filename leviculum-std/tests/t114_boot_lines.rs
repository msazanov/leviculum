//! The T114's boot lines, pinned across its move onto the shared node loop
//! (#469 step 7, order 1085).
//!
//! The move puts about 700 lines of `leviculum-nrf/src/bin/t114.rs` into
//! `leviculum-nrf/src/node_loop.rs`, and the binary keeps only the board's
//! wiring between the skeleton's calls. Nothing a reader of the debug port
//! sees may change beyond the two decisions the order applies (D2, D7, Lew
//! 2026-10-28): the boot banner lines and their order, the `[STG]` stage
//! lines and the `BOOT_TRACE` phase stamps are what the rig cells and the
//! recovery doc key on, and the firmware cross-compiles to thumbv7em, so no
//! host test can run the boot itself.
//!
//! So the test reads the boot as TEXT, in the order the board runs it: the
//! binary's `main`, with every `node_loop::<step>(` call replaced by the
//! lines of that step's body, then the skeleton's tasks and helpers. From
//! that text it takes every line a board writes (`log_critical!`, `info!`,
//! `warn!`, `log_fmt`/`log_fmt_critical` with their prefix, every call of a
//! `log_*` helper, every `boot_trace` `Phase::`) in order, and compares the
//! sequence with [`PINNED_PRE_1085`]: the same extraction over the binary as
//! it stood at 40836451, before the move, with the two decided changes
//! applied by name in [`expected`].

use regex::Regex;
use std::path::{Path, PathBuf};

/// Every line `bin/t114.rs` wrote at 40836451, in source order, as the
/// extraction below spells it. Generated once from that file; a change here
/// is a change to what the T114 says at boot and needs its own decision.
const PINNED_PRE_1085: &[&str] = &[
    "phase PersistRead",
    "phase UsbUp",
    "log_critical leviculum T114 booting",
    "log_critical [FW_BUILD] {}",
    "log_critical [TIME_SOURCE] source={}",
    "call log_boot_diagnostics",
    "log_critical Identity loaded from flash",
    "log_critical No identity in flash, generating new",
    "call heap_census::log_budget_and_assert",
    "log_critical [TIME_SOURCE] source={}",
    "call mgmt::log_banner",
    "info [BOOT] path_table_initial_len={}",
    "info Identity saved to flash",
    "log_critical LNode started -- identity: {:02X}{:02X}{:02X}{:02X}{:02X}",
    "log_fmt_critical [QSPI] NONE board=t114 reason=not-fitted-see-boards-t114-rs",
    "log_critical [STG] lora-init",
    "info SX1262 ready",
    "log_fmt [RADIO] persisted freq={} bw={} sf={} cr={} pwr={}",
    "log_fmt [RADIO] default eu_medium",
    "phase LoraTask",
    "call media::log_carrier_held_down",
    "phase LoraSkipped",
    "log_critical [STG] ble-init",
    "info BLE ready",
    "call media::log_banner",
    "call name::log_banner",
    "log_critical [STG] display-spawn",
    "info display task spawned (ST7789 blind-drive)",
    "info gnss task spawned (L76K)",
    "info battery task spawned",
    "info heap u={} f={}",
    "call log_stack",
    "log_critical PN role=off reason=identity-underivable",
    "log_critical [TELEMETRY] target=00000000 state=off reason=no-delivery-destination",
    "call reporter.log_banner",
    "log_critical [TIME_SEED_REFUSED] source=gnss reason=watch_capacity",
    "log_critical [STG] main-loop",
    "phase MainLoop",
    "call events::log_events",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED] source=gnss unix={}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED_REFUSED] source=gnss unix={}",
    "log_critical [TIME_SEED] source=host unix={}",
    "log_critical [TIME_SOURCE] source={}",
    "log_critical [TIME_SEED_REFUSED] source=host unix={}",
    "call reporter.log_banner",
    "log_critical [TELEMETRY] fixed-position set lat_e6={} lon_e6={} alt_e2={} alt_present={}",
    "log_critical [TELEMETRY] fixed-position cleared",
    "log_critical [ANNOUNCE] withheld reason=no-clock",
    "log_critical [ANNOUNCE] sent dst={:02x}{:02x}{:02x}{:02x} reason=host",
    "info SER RX {} bytes",
    "info SER RX -> {} actions",
    "info LORA RX -> {} actions",
    "info BLE RX -> {} actions",
    "info BLE peer lost, {} paths culled",
    "info BLE peer up, {} pull actions",
    "info timeout: {} actions",
    "info [BOOT] path_table_initial_len={}",
];

/// The pinned sequence with the order's two decisions applied.
fn expected() -> Vec<String> {
    let mut lines: Vec<String> = PINNED_PRE_1085.iter().map(|s| s.to_string()).collect();
    // D2: every GNSS board prints the `[GNSS_PRESENCE] state=detecting`
    // anchor from `gnss.rs`, after the first `[TIME_SOURCE]` and before the
    // boot diagnostics, where the RAK4631 has always printed it.
    let first_time_source = lines
        .iter()
        .position(|l| l == "log_critical [TIME_SOURCE] source={}")
        .expect("pinned list has a [TIME_SOURCE] line");
    lines.insert(
        first_time_source + 1,
        "call gnss::log_detecting".to_string(),
    );
    // D7: the spawn lines say `<task> task spawned (<module>)`, the module
    // named by the board file; "blind-drive" was the T114's alone.
    let display = lines
        .iter()
        .position(|l| l == "info display task spawned (ST7789 blind-drive)")
        .expect("pinned list has the display spawn line");
    lines[display] = "info display task spawned (ST7789)".to_string();
    lines
}

fn tree(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `src` without its `//` comments (doc comments included); string and
/// char literals are kept whole, so a `//` or a brace inside one survives.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str(&src[start..i]);
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'\'' && b.get(i + 2) == Some(&b'\'') {
            out.push_str(&src[i..i + 3]);
            i += 3;
        } else {
            let ch = src[i..].chars().next().expect("in bounds");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// The byte span of `fn <name>(`'s body, braces included, in stripped text.
/// Braces inside string literals do not count.
fn fn_body_span(src: &str, name: &str) -> (usize, usize) {
    let sig = Regex::new(&format!(r"\bfn {name}\b")).expect("regex");
    let at = sig
        .find(src)
        .unwrap_or_else(|| panic!("no `fn {name}` in the source"))
        .start();
    let b = src.as_bytes();
    let open = at + src[at..].find('{').expect("fn has a body");
    let mut depth = 0usize;
    let mut i = open;
    loop {
        match b[i] {
            b'"' => {
                i += 1;
                while b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (open, i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Every line `src` writes, with its byte offset, in source order.
fn lines_written(src: &str) -> Vec<(usize, String)> {
    let lit = r#""((?:[^"\\]|\\.)*)""#;
    let mac = Regex::new(&format!(r"\b(log_critical|info|warn)!\(\s*{lit}")).expect("regex");
    let fmt = Regex::new(&format!(
        r"\b(log_fmt_critical|log_fmt)\(\s*{lit},\s*format_args!\(\s*{lit}"
    ))
    .expect("regex");
    let phase = Regex::new(r"Phase::(\w+)").expect("regex");
    let call = Regex::new(r"(?:(\w+)(::|\.))?\b(log_\w+)\(").expect("regex");
    let mut out = Vec::new();
    for c in mac.captures_iter(src) {
        let at = c.get(0).expect("match").start();
        out.push((at, format!("{} {}", &c[1], &c[2])));
    }
    for c in fmt.captures_iter(src) {
        let at = c.get(0).expect("match").start();
        out.push((at, format!("{} {}{}", &c[1], &c[2], &c[3])));
    }
    for c in phase.captures_iter(src) {
        let at = c.get(0).expect("match").start();
        out.push((at, format!("phase {}", &c[1])));
    }
    for c in call.captures_iter(src) {
        let name = &c[3];
        let at = c.get(0).expect("match").start();
        // A definition is not a line, `log_fmt` is the shape above, and a
        // `node_loop::` step is spliced by `boot_lines`.
        if name == "log_fmt"
            || name == "log_fmt_critical"
            || src[..at].ends_with("fn ")
            || c.get(1).is_some_and(|m| m.as_str() == "node_loop")
        {
            continue;
        }
        let prefix = match c.get(1).map(|m| m.as_str()) {
            None | Some("crate") | Some("leviculum_nrf") => String::new(),
            Some(module) => format!("{module}{}", &c[2]),
        };
        out.push((at, format!("call {prefix}{name}")));
    }
    out.sort_by_key(|(at, _)| *at);
    out
}

/// The T114's boot in the order it runs, from the binary, the skeleton and
/// the board file (all three raw source text).
fn boot_lines(bin: &str, skeleton: &str, board: &str) -> Vec<String> {
    let bin = strip_comments(bin);
    let skeleton = strip_comments(skeleton);
    let step = Regex::new(r"node_loop::(\w+)\(").expect("regex");
    let spawned = Regex::new(
        r#"^node_loop::log_task_spawned\(\s*"(\w+)",\s*(?:Some\(t114::(\w+)\)|None)\s*\)"#,
    )
    .expect("regex");

    let (main_start, main_end) = fn_body_span(&bin, "main");
    let main = &bin[main_start..main_end];
    let mut events: Vec<(usize, Vec<String>)> = lines_written(main)
        .into_iter()
        .map(|(at, line)| (at, vec![line]))
        .collect();
    let mut spliced: Vec<(usize, usize)> = Vec::new();
    for c in step.captures_iter(main) {
        let at = c.get(0).expect("match").start();
        let name = &c[1];
        if name == "log_task_spawned" {
            // The one step whose line depends on its arguments: render it
            // the way the function does, with the module from the board file.
            spliced.push(fn_body_span(&skeleton, name));
            let a = spawned
                .captures(&main[at..])
                .unwrap_or_else(|| panic!("unreadable log_task_spawned call in bin/t114.rs"));
            let line = match a.get(2) {
                Some(module) => {
                    let konst = Regex::new(&format!(
                        r#"pub const {}: &str = "([^"]*)";"#,
                        module.as_str()
                    ))
                    .expect("regex");
                    let value = konst
                        .captures(board)
                        .unwrap_or_else(|| panic!("boards/t114.rs has no {}", module.as_str()));
                    format!("info {} task spawned ({})", &a[1], &value[1])
                }
                None => format!("info {} task spawned", &a[1]),
            };
            events.push((at, vec![line]));
        } else {
            let span = fn_body_span(&skeleton, name);
            spliced.push(span);
            let body = lines_written(&skeleton[span.0..span.1])
                .into_iter()
                .map(|(_, l)| l)
                .collect();
            events.push((at, body));
        }
    }
    events.sort_by_key(|(at, _)| *at);

    let mut lines: Vec<String> = lines_written(&bin[..main_start])
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    lines.extend(events.into_iter().flat_map(|(_, l)| l));
    lines.extend(lines_written(&bin[main_end..]).into_iter().map(|(_, l)| l));
    // The skeleton's tasks and helpers no binary calls by name, after the
    // binary, where the old file had them after `main`.
    spliced.sort();
    spliced.dedup();
    let mut rest = String::new();
    let mut from = 0;
    for (s, e) in spliced {
        if s >= from {
            rest.push_str(&skeleton[from..s]);
            from = e;
        }
    }
    rest.push_str(&skeleton[from..]);
    lines.extend(lines_written(&rest).into_iter().map(|(_, l)| l));
    lines
}

fn sources() -> (String, String, String) {
    (
        tree("leviculum-nrf/src/bin/t114.rs"),
        tree("leviculum-nrf/src/node_loop.rs"),
        tree("leviculum-nrf/src/boards/t114.rs"),
    )
}

fn mismatch(got: &[String], want: &[String]) -> Option<String> {
    if got == want {
        return None;
    }
    let first = got
        .iter()
        .zip(want)
        .position(|(g, w)| g != w)
        .unwrap_or(got.len().min(want.len()));
    Some(format!(
        "first difference at line {first}: got {:?}, want {:?} ({} lines got, {} want)",
        got.get(first),
        want.get(first),
        got.len(),
        want.len()
    ))
}

#[test]
fn the_t114_boot_lines_are_the_pre_move_lines_plus_d2_and_d7() {
    let (bin, skeleton, board) = sources();
    let got = boot_lines(&bin, &skeleton, &board);
    if let Some(diff) = mismatch(&got, &expected()) {
        panic!(
            "the T114's boot lines moved (order 1085 pins them): {diff}\nfull sequence:\n{}",
            got.join("\n")
        );
    }
}

/// Positive control: one line dropped from the skeleton, one step call
/// dropped from the binary, and one line moved, each turn the comparison
/// red.
#[test]
fn a_dropped_line_turns_the_pin_red() {
    let (bin, skeleton, board) = sources();
    let drop_line = |src: &str, needle: &str| -> String {
        assert!(src.contains(needle), "control needle {needle:?} is gone");
        src.lines()
            .filter(|l| !l.contains(needle))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let want = expected();
    let no_stage = drop_line(&skeleton, r#"log_critical!("[STG] ble-init");"#);
    assert!(
        mismatch(&boot_lines(&bin, &no_stage, &board), &want).is_some(),
        "dropping `[STG] ble-init` from the skeleton left the pin green"
    );
    let no_banner = drop_line(&bin, "node_loop::boot_banner(");
    assert!(
        mismatch(&boot_lines(&no_banner, &skeleton, &board), &want).is_some(),
        "dropping the boot banner step from the binary left the pin green"
    );
    // Order, not only the set: the stage line moved behind the line it
    // brackets keeps every line and still turns the pin red.
    let stage = r#"    log_critical!("[STG] lora-init");"#;
    let ready = r#"    info!("SX1262 ready");"#;
    assert!(
        bin.contains(stage) && bin.contains(ready),
        "control lines are gone"
    );
    let reordered = bin
        .replacen(stage, "", 1)
        .replacen(ready, &format!("{ready}\n{stage}"), 1);
    assert!(
        mismatch(&boot_lines(&reordered, &skeleton, &board), &want).is_some(),
        "moving `[STG] lora-init` behind `SX1262 ready` left the pin green"
    );
}

/// The extraction itself, on the pre-move binary's shape: a line inside a
/// comment is not a line, a `log_fmt` keeps its prefix, a definition is not
/// a call.
#[test]
fn the_extraction_reads_what_a_board_writes() {
    let src = "// log_critical!(\"commented\");\nlog_critical!(\"a {}\", x);\nleviculum_nrf::log::log_fmt(\"[R] \", format_args!(\"b={}\", y));\nfn log_banner() {}\nmedia::log_banner(z);\nboot_trace::phase(Phase::UsbUp);\n";
    let got: Vec<String> = lines_written(&strip_comments(src))
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    assert_eq!(
        got,
        [
            "log_critical a {}",
            "log_fmt [R] b={}",
            "call media::log_banner",
            "phase UsbUp"
        ]
    );
}
