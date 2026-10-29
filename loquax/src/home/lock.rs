//! One registrar of the delivery destination per loquax home.
//!
//! The messenger and `loquax --daemon` both register the home identity's
//! delivery destination with the shared instance. Two of them on one home
//! would answer the same senders from two processes, each archiving or
//! showing half the mail. So each takes this lock first, an advisory
//! `flock` on `$LNMSG_HOME/delivery.lock`, and the second refuses to start.
//! The kernel drops the lock with the process, so a crash leaves nothing to
//! clean up.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// The lock's file under the loquax home.
pub const LOCK_FILE: &str = "delivery.lock";

/// The held lock; dropping it lets go.
#[derive(Debug)]
pub struct DeliveryLock {
    _file: File,
}

impl DeliveryLock {
    /// Take the lock for `home`, for a process that is `what` (`the
    /// messenger`, `the home node`). Refused while another process holds
    /// it; the file says which.
    pub fn acquire(home: &Path, what: &str) -> Result<Self, String> {
        let path = home.join(LOCK_FILE);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&path).unwrap_or_default();
                let holder = holder.trim();
                let holder = match holder.is_empty() {
                    true => "another loquax",
                    false => holder,
                };
                return Err(format!(
                    "{holder} already runs on this loquax home ({}), and two would \
                     register the same delivery destination; stop it first",
                    path.display()
                ));
            }
            Err(TryLockError::Error(error)) => return Err(format!("{}: {error}", path.display())),
        }
        // Who holds it, for the refusal another process prints.
        let said = format!("{what} (pid {})\n", std::process::id());
        let _ = file
            .set_len(0)
            .and_then(|()| std::io::Write::write_all(&mut file, said.as_bytes()));
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_registrar_on_one_home_is_refused_until_the_first_lets_go() {
        let home = tempfile::tempdir().expect("home");
        let first = DeliveryLock::acquire(home.path(), "the home node").expect("first");
        let refused = DeliveryLock::acquire(home.path(), "the messenger").expect_err("second");
        assert!(refused.contains("the home node (pid"), "{refused}");
        assert!(refused.contains("already runs"), "{refused}");
        // Another home is another lock.
        let other = tempfile::tempdir().expect("other home");
        DeliveryLock::acquire(other.path(), "the messenger").expect("other home");
        drop(first);
        DeliveryLock::acquire(home.path(), "the messenger").expect("after the first let go");
    }
}
