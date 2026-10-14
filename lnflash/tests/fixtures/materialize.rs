//! Decode the committed sysfs fixture tree into something tests can walk.
//!
//! Sysfs names carry colons — interface directories (`3-2.3.1:1.0`), SCSI
//! addresses (`target4:0:0`) — and a colon cannot be checked out on NTFS,
//! so a tree committed verbatim makes `git clone` (and every cargo fetch of
//! this repo as a git dependency) fail on Windows before a single crate
//! builds. The committed tree therefore encodes `:` as `+`, and tests copy
//! it into a temp directory with the real names restored before handing it
//! to [`Sysfs`]. `+` appears in no real sysfs name, and the enumeration
//! code never sees the encoding — it walks a tree with true colons, same
//! as on the rig.
//!
//! Shared by the unit tests (via `#[path]` in `lib.rs`) and `tests/cli.rs`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static TREE: OnceLock<PathBuf> = OnceLock::new();

/// The fixture tree with sysfs names restored, built once per test process.
///
/// A static is never dropped, so a `TempDir` held here would outlive the run
/// exactly as the fixed `lnflash-sysfs-<pid>` directory before it did, one
/// per test binary per run (730). The tree is removed by an `atexit` hook
/// instead, which the test harness reaches on every ordinary exit, a failed
/// test included.
pub fn materialized() -> &'static Path {
    TREE.get_or_init(|| {
        let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
        let dst = tempfile::Builder::new()
            .prefix("lnflash-sysfs-")
            .tempdir()
            .expect("temp dir for the sysfs fixture tree")
            .keep();
        copy_decoded(src, &dst).expect("materialize sysfs fixture tree");
        // SAFETY: registers a plain `extern "C" fn` with no captured state;
        // it only reads `TREE`, which is set by the time this returns.
        unsafe { libc::atexit(remove_tree) };
        dst
    })
}

extern "C" fn remove_tree() {
    if let Some(tree) = TREE.get() {
        let _ = std::fs::remove_dir_all(tree);
    }
}

/// A private copy of the fixture tree, for tests that mutate the bus — a
/// board re-enumerating, a device disappearing. The shared [`materialized`]
/// tree must never be mutated: every other test in the process walks it.
#[allow(dead_code)] // each includer uses its own subset of this module
pub fn materialized_copy(dst: &Path) {
    let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sysfs"));
    copy_decoded(src, dst).expect("materialize private sysfs fixture tree");
}

fn copy_decoded(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| io::Error::other("non-UTF-8 fixture name"))?;
        let target = dst.join(name.replace('+', ":"));
        if entry.file_type()?.is_dir() {
            copy_decoded(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
