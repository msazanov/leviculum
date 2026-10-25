//! The toast model: a transient note floated over the content. Its expiry is
//! the program's (counted in its own ticks); its drawing is
//! `leviculum-ui-ratatui`'s.

use alloc::string::String;

/// Whether a transient toast carries an error or a neutral/success note. Drives
/// the toast's accent colour and glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// A failure the reader should notice (a bad link, a failed fetch): drawn in
    /// an attention colour with a `⚠` prefix.
    Error,
    /// A neutral confirmation (copied, bookmarked, cancelled): drawn in a calm
    /// colour with a `✓` prefix.
    Info,
}

/// A transient, auto-dismissing notification floated over the content. In
/// lnomad it replaced the old sticky status-bar messages, leaving the status
/// bar for the key-hints (or the loading spinner during a fetch).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// Whether this is an error or a neutral note.
    pub kind: ToastKind,
    /// The message text; carries the offending target for error toasts so it is
    /// self-describing.
    pub text: String,
    /// The program's tick counter when the toast was shown (lnomad's
    /// `Model::now_tick`), so its age (and thus expiry) is a pure function of a
    /// monotonic tick counter and needs no wall clock to unit-test.
    pub shown_at: u64,
}
