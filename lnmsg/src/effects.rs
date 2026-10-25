//! What the core asks its host to do with what a run leaves behind.
//!
//! The send and fetch runners are core: they decide what the person running
//! `lnmsg` is told along the way and what is kept for the next run. Doing it
//! is the shell's, because it means a terminal and files: the stderr lines,
//! the mail on stdout, `${LNMSG_HOME}/mailbox` and `${LNMSG_HOME}/seen`. [`Effects`] is that
//! boundary, so a board or a GUI runs the same runners with its own screen and
//! its own storage (`docs/src/concepts/lnmsg-architecture.md`, "Plan: the
//! lnmsg core without std", pass 2).
//!
//! The core hands over finished values, never a path or an `io::Error`: the
//! notice is the line to show, the mailbox selection is a [`Stored`] that
//! serialises itself ([`Stored::to_text`]), the seen record is a
//! [`SeenWrite`]. A failure comes back as its text, and the core words the
//! notice around it, so every host says the same thing.

use alloc::string::String;

use crate::mailbox_state::Stored;
use crate::seen::SeenWrite;

/// The host side of a run.
pub trait Effects {
    /// Show one line to the person running `lnmsg`. The terminal writes it to
    /// stderr behind `lnmsg: `.
    fn notice(&mut self, line: &str);
    /// Print one rendered message where the person running `lnmsg` reads
    /// mail, whole, in one write. The terminal writes it to stdout and
    /// flushes. A failure comes back as its text.
    fn print(&mut self, rendered: &[u8]) -> Result<(), String>;
    /// Keep the mailbox selection for the next run.
    fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String>;
    /// Apply one change to the seen record.
    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String>;
    /// The wall clock in Unix milliseconds, the path table's timescale.
    fn unix_now_ms(&self) -> u64;
}

/// The terminal's effects: stderr, and the files under `${LNMSG_HOME}`.
#[cfg(feature = "std")]
pub struct Terminal {
    home: std::path::PathBuf,
}

#[cfg(feature = "std")]
impl Terminal {
    /// Effects that keep their files in `home` (`identity::home_dir`).
    pub fn new(home: std::path::PathBuf) -> Self {
        Self { home }
    }
}

#[cfg(feature = "std")]
impl Effects for Terminal {
    fn notice(&mut self, line: &str) {
        eprintln!("lnmsg: {line}");
    }

    fn print(&mut self, rendered: &[u8]) -> Result<(), String> {
        use std::io::Write;
        // Deliberately not `stdout().lock()` held across the run: see
        // `run_fetch_command` in `main.rs`. `write_all` takes the lock for
        // this one message only.
        let mut stdout = std::io::stdout();
        stdout
            .write_all(rendered)
            .and_then(|()| stdout.flush())
            .map_err(|error| error.to_string())
    }

    fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String> {
        crate::mailbox_state::save(&self.home, stored).map_err(|error| error.to_string())
    }

    fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
        crate::seen::apply(&self.home, write).map_err(|error| error.to_string())
    }

    fn unix_now_ms(&self) -> u64 {
        crate::mailbox_state::unix_now_ms()
    }
}

/// Effects that keep everything they were asked to do, for the runners' tests.
#[cfg(test)]
pub(crate) mod fake {
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    use super::Effects;
    use crate::mailbox_state::Stored;
    use crate::seen::SeenWrite;

    #[derive(Default)]
    pub struct Recorded {
        pub notices: Vec<String>,
        /// Every rendered message, concatenated as stdout would hold them.
        pub printed: Vec<u8>,
        pub mailboxes: Vec<Stored>,
        pub seen: Vec<SeenWrite>,
        /// Refuse every seen write with this text, as a full disk would.
        pub seen_fails: Option<&'static str>,
        pub now_ms: u64,
    }

    impl Effects for Recorded {
        fn notice(&mut self, line: &str) {
            self.notices.push(line.to_string());
        }

        fn print(&mut self, rendered: &[u8]) -> Result<(), String> {
            self.printed.extend_from_slice(rendered);
            Ok(())
        }

        fn save_mailbox(&mut self, stored: &Stored) -> Result<(), String> {
            self.mailboxes.push(*stored);
            Ok(())
        }

        fn write_seen(&mut self, write: &SeenWrite) -> Result<(), String> {
            if let Some(error) = self.seen_fails {
                return Err(error.to_string());
            }
            self.seen.push(write.clone());
            Ok(())
        }

        fn unix_now_ms(&self) -> u64 {
            self.now_ms
        }
    }
}
