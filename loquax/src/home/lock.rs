//! One registrar of the delivery destination per loquax home.
//!
//! The messenger and `loquax --daemon` both register the home identity's
//! delivery destination with the shared instance. Two of them on one home
//! would answer the same senders from two processes, each archiving or
//! showing half the mail. So each takes this lock first, an advisory
//! `flock` on `$LNMSG_HOME/delivery.lock`, and the second refuses to start.
//! The kernel drops the lock with the process, so a crash leaves nothing to
//! clean up.
//!
//! `loquax send` and `loquax fetch` register it as well, for their run.
//! They take the lock shared ([`DeliveryLock::share`]): two of them run
//! side by side as they always did, and none runs while the home node does,
//! nor does the home node start under one. Beside the messenger they run
//! unlocked, as before the lock existed: a script that sends while the
//! screen is open is a use the messenger has, and the home node's is not.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// The lock's file under the loquax home.
pub const LOCK_FILE: &str = "delivery.lock";

/// What the home node calls itself in the lock file.
pub const HOME_NODE: &str = "Loquax Mailbox";

/// What it called itself before 1153. A mailbox started by an older binary
/// still holds the lock under this name through an upgrade, and a send
/// beside it has to be refused all the same.
const LEGACY_HOME_NODE: &str = "the home node";

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
                // Held shared: only sends and fetches, which wrote nothing,
                // so the file still names whoever held it last.
                let holder = match file.try_lock_shared() {
                    Ok(()) => {
                        let _ = file.unlock();
                        "a loquax send or fetch".to_string()
                    }
                    Err(_) => holder(&path),
                };
                return Err(format!(
                    "{holder} already runs on this Loquax home ({}), and two would \
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

    /// Take the lock shared for `loquax send` or `loquax fetch` on `home`.
    /// Refused while the home node holds it, naming its pid. `None` while
    /// the messenger holds it: the run goes on unlocked.
    pub fn share(home: &Path) -> Result<Option<Self>, String> {
        let path = home.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        match file.try_lock_shared() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => {
                let holder = holder(&path);
                match holder.starts_with(HOME_NODE) || holder.starts_with(LEGACY_HOME_NODE) {
                    true => Err(format!(
                        "{holder} runs on this Loquax home ({}) and answers for its \
                         identity; send and fetch from one of its clients, or stop it first",
                        path.display()
                    )),
                    false => Ok(None),
                }
            }
            Err(TryLockError::Error(error)) => Err(format!("{}: {error}", path.display())),
        }
    }
}

/// Who holds the lock exclusively, as the holder wrote it.
fn holder(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    match text.trim() {
        "" => "another Loquax".to_string(),
        said => said.to_string(),
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

    #[test]
    fn send_and_fetch_share_the_lock_and_the_home_node_excludes_them() {
        let home = tempfile::tempdir().expect("home");
        // Two sends side by side.
        let one = DeliveryLock::share(home.path())
            .expect("one")
            .expect("held");
        let two = DeliveryLock::share(home.path())
            .expect("two")
            .expect("held");
        // The home node does not start under them, and says why.
        let refused = DeliveryLock::acquire(home.path(), HOME_NODE).expect_err("under a send");
        assert!(refused.contains("a loquax send or fetch"), "{refused}");
        drop((one, two));
        let node = DeliveryLock::acquire(home.path(), HOME_NODE).expect("home node");
        let refused = DeliveryLock::share(home.path()).expect_err("beside the home node");
        assert!(
            refused.contains(&format!("Loquax Mailbox (pid {})", std::process::id())),
            "{refused}"
        );
        drop(node);
        // Beside the messenger a send runs unlocked.
        let messenger = DeliveryLock::acquire(home.path(), "the messenger").expect("messenger");
        assert!(DeliveryLock::share(home.path())
            .expect("beside it")
            .is_none());
        drop(messenger);
    }

    /// 1153: a mailbox an older binary started names itself as it did then,
    /// and a send beside it is still refused through the upgrade.
    #[test]
    fn a_mailbox_under_its_old_name_still_excludes_a_send() {
        let home = tempfile::tempdir().expect("home");
        let node = DeliveryLock::acquire(home.path(), LEGACY_HOME_NODE).expect("old mailbox");
        let refused = DeliveryLock::share(home.path()).expect_err("beside the old mailbox");
        assert!(refused.contains("the home node (pid"), "{refused}");
        drop(node);
    }
}
