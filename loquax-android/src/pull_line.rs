//! What the status line says about a pull (1136). A wake comes for every
//! message, state and mark the home node archives, and a line that said
//! "pulling" and then a count for each flickered without end. So a pull the
//! user asked for (Refresh) says it runs at once; any other says so only
//! once it took longer than [`SLOW_MS`]. Its end is said when its start
//! was, or when it brought a message; a quiet pull that brought only names,
//! states or marks says nothing. Pure: the session feeds it the clock.

/// How long a quiet pull runs before the line says so.
pub const SLOW_MS: u64 = 1_000;

/// What the line is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Say {
    /// A pull runs.
    Running,
    /// A pull ended and brought this many new messages.
    Done(usize),
    /// A pull failed, and why.
    Failed(String),
}

/// The running pull, if one runs.
#[derive(Debug, Default)]
pub struct PullLine {
    running: Option<Running>,
}

#[derive(Debug)]
struct Running {
    since_ms: u64,
    shown: bool,
}

impl PullLine {
    /// A pull began at `now_ms`; `asked` when the user asked for it.
    pub fn began(&mut self, now_ms: u64, asked: bool) -> Option<Say> {
        self.running = Some(Running {
            since_ms: now_ms,
            shown: asked,
        });
        asked.then_some(Say::Running)
    }

    /// The user asked for a pull while one runs: that one is theirs now.
    pub fn asked(&mut self) -> Option<Say> {
        let running = self.running.as_mut()?;
        (!std::mem::replace(&mut running.shown, true)).then_some(Say::Running)
    }

    /// The clock at `now_ms`: a quiet pull past [`SLOW_MS`] is said now.
    pub fn tick(&mut self, now_ms: u64) -> Option<Say> {
        let running = self.running.as_mut()?;
        if running.shown || now_ms.saturating_sub(running.since_ms) < SLOW_MS {
            return None;
        }
        running.shown = true;
        Some(Say::Running)
    }

    /// The pull ended with `result`, the count of new messages or why not.
    pub fn ended(&mut self, result: &Result<usize, String>) -> Option<Say> {
        let shown = self.running.take().is_some_and(|running| running.shown);
        match result {
            Ok(new) if shown || *new > 0 => Some(Say::Done(*new)),
            Err(error) if shown => Some(Say::Failed(error.clone())),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_pull_with_nothing_new_says_nothing() {
        let mut line = PullLine::default();
        assert_eq!(line.began(0, false), None);
        assert_eq!(line.tick(SLOW_MS - 1), None);
        assert_eq!(line.ended(&Ok(0)), None);
        assert_eq!(line.ended(&Err("gone".to_string())), None, "none runs");
    }

    #[test]
    fn a_wake_pull_that_brings_a_message_says_only_the_result() {
        let mut line = PullLine::default();
        assert_eq!(line.began(0, false), None);
        assert_eq!(line.ended(&Ok(1)), Some(Say::Done(1)));
    }

    #[test]
    fn a_slow_pull_says_it_runs_once_and_then_how_it_ended() {
        let mut line = PullLine::default();
        line.began(100, false);
        assert_eq!(line.tick(100 + SLOW_MS), Some(Say::Running));
        assert_eq!(line.tick(100 + 2 * SLOW_MS), None, "once");
        assert_eq!(line.ended(&Ok(0)), Some(Say::Done(0)));
        line.began(0, false);
        line.tick(SLOW_MS);
        assert_eq!(
            line.ended(&Err("no path".to_string())),
            Some(Say::Failed("no path".to_string()))
        );
    }

    #[test]
    fn a_refresh_says_it_runs_at_once_and_how_it_ended() {
        let mut line = PullLine::default();
        assert_eq!(line.began(0, true), Some(Say::Running));
        assert_eq!(line.tick(5 * SLOW_MS), None);
        assert_eq!(line.ended(&Ok(0)), Some(Say::Done(0)));
        // Asked while a quiet one runs: that one is said.
        line.began(0, false);
        assert_eq!(line.asked(), Some(Say::Running));
        assert_eq!(line.asked(), None);
        assert_eq!(line.ended(&Ok(0)), Some(Say::Done(0)));
        assert_eq!(line.asked(), None, "none runs");
    }
}
