//! Codeberg #462 — `known_destinations` carries the announce time, read by
//! the reference both ways.
//!
//! The first element of a known-destination entry is the time the identity
//! was remembered: `Identity.remember` stamps `time.time()` there on every
//! announce (`reference/Reticulum/RNS/Identity.py:107`, `:109`), and the cull
//! reads it back as `last_announce` (`Identity.py:331`, `:351`) to drop a
//! never-used destination that has stopped announcing. Our flush wrote the
//! flush time into that element for every identity it held, so an `rnsd` that
//! took over our file saw every destination as freshly announced and culled
//! none of them.
//!
//! Both directions run against the pinned `reference/Reticulum`, offline: no
//! daemon, only the file. Python's own loader and its own `time.time()` turn
//! the first element into an age, which is exactly the subtraction the cull
//! does.
//!
//! Not `#[ignore]`d: Python here only reads and writes a file, no daemon
//! and no socket, and the two tests take about six seconds between them, so
//! they ride the suite like the rest of it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use leviculum_core::traits::Storage as CoreStorage;
use leviculum_std::driver::StdStorage;

/// Pinned reference package, as every interop tool in this suite uses it.
const VENDOR_RNS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../reference/Reticulum");

/// Python side, both directions. `ages` loads the file through
/// `Identity.load_known_destinations` and prints `now - entry[0]` per entry,
/// the cull's `now - last_announce`. `write` remembers fresh identities
/// through `Identity.remember`, backdates each first element by the given
/// ages, and saves through `Identity.save_known_destinations`.
const AGES_PY: &str = r#"
import sys, time
import RNS

storage = sys.argv[1]
RNS.Reticulum.storagepath = storage

if sys.argv[2] == "ages":
    RNS.Identity.load_known_destinations()
    now = time.time()
    print("NOW " + repr(now))
    for dest, entry in RNS.Identity.known_destinations.items():
        print("AGE " + dest.hex() + " " + repr(now - entry[0]))
elif sys.argv[2] == "write":
    now = time.time()
    for age in sys.argv[3:]:
        identity = RNS.Identity()
        dest = RNS.Destination.hash(identity, "ageinterop", "rnsd")
        RNS.Identity.remember(RNS.Identity.get_random_hash(), dest, identity.get_public_key())
        RNS.Identity.known_destinations[dest][0] = now - float(age)
        print("STAMP " + dest.hex() + " " + repr(now - float(age)))
    RNS.Identity.save_known_destinations()
else:
    raise SystemExit("unknown op: " + sys.argv[2])
"#;

/// Float rounding on a subtraction of two Unix times near 1.8e9 s, where one
/// ulp is 2.4e-7 s. Nothing else: both sides of the comparison are taken
/// against the `now` Python printed, and our side is a window bracketing
/// the remember, so no start-up or scheduling delay enters it. Under a 25 %
/// CPU quota the throttle once put 146 ms between a clock read and the
/// `set_identity` right after it, which a fixed slack would have had to
/// absorb.
const FLOAT_EPS_SECS: f64 = 1e-5;

/// Gap between two remembers, so the flush time and the remember time cannot
/// be mistaken for each other.
const REMEMBER_GAP: Duration = Duration::from_secs(3);

fn unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn run_python(storage: &Path, args: &[&str]) -> String {
    let script = storage.join("known_destinations_ages.py");
    std::fs::write(&script, AGES_PY).expect("write the python side");
    let output = std::process::Command::new("python3")
        .arg(&script)
        .arg(storage)
        .args(args)
        .env("PYTHONPATH", VENDOR_RNS_ROOT)
        .output()
        .expect("spawn python3");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "python {args:?} must exit 0, got {:?}\nstdout {stdout}\nstderr {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    stdout
}

/// `<tag> <hex> <float>` lines, keyed by destination hash.
fn tagged(stdout: &str, tag: &str) -> BTreeMap<[u8; 16], f64> {
    let mut out = BTreeMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(tag) {
            continue;
        }
        let hex_hash = parts.next().expect("a hash after the tag");
        let value: f64 = parts
            .next()
            .expect("a number after the hash")
            .parse()
            .expect("a float");
        let bytes = hex::decode(hex_hash).expect("hex hash");
        let mut hash = [0u8; 16];
        hash.copy_from_slice(&bytes);
        out.insert(hash, value);
    }
    out
}

/// The `now` Python computed its ages against.
fn python_now(stdout: &str) -> f64 {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("NOW "))
        .unwrap_or_else(|| panic!("no NOW line in {stdout}"))
        .trim()
        .parse()
        .expect("a float")
}

/// The first element per entry, as our file holds it.
fn file_stamps(storage: &Path) -> BTreeMap<[u8; 16], f64> {
    let bytes = std::fs::read(storage.join("known_destinations")).expect("a persisted file");
    let value = rmpv::decode::read_value(&mut &bytes[..]).expect("msgpack map");
    let mut out = BTreeMap::new();
    for (key, val) in value.as_map().expect("a map, as the reference writes") {
        let mut hash = [0u8; 16];
        hash.copy_from_slice(key.as_slice().expect("binary key"));
        let stamp = val.as_array().expect("array entry")[0]
            .as_f64()
            .expect("a float first element");
        out.insert(hash, stamp);
    }
    out
}

/// Where an announce time must lie: the clock read just before the
/// remember and the one just after. A stamp rnsd wrote is a window of zero
/// width.
#[derive(Clone, Copy)]
struct Window {
    before: f64,
    after: f64,
}

impl Window {
    /// Whether `age`, taken by Python at `now`, is an age of this window.
    fn holds_age(self, now: f64, age: f64) -> bool {
        now - self.after - FLOAT_EPS_SECS <= age && age <= now - self.before + FLOAT_EPS_SECS
    }
}

/// Remember a fresh identity; its hash and the window around the remember.
fn remember(storage: &mut StdStorage) -> ([u8; 16], Window) {
    let identity = leviculum_std::generate_identity();
    let mut hash = [0u8; 16];
    hash.copy_from_slice(&identity.hash()[..16]);
    let before = unix_secs();
    CoreStorage::set_identity(storage, hash, identity);
    let after = unix_secs();
    (hash, Window { before, after })
}

/// Ours to theirs: three identities remembered three seconds apart, each
/// followed by a flush, the way the daemon's interval flush interleaves with
/// announces. The reference must read each one's age from its remember, not
/// from the last flush.
#[test]
fn rnsd_reads_the_announce_age_from_our_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut remembered_at = BTreeMap::new();
    {
        let mut storage = StdStorage::new(dir.path()).expect("storage");
        for round in 0..3 {
            if round > 0 {
                std::thread::sleep(REMEMBER_GAP);
            }
            let (hash, at) = remember(&mut storage);
            remembered_at.insert(hash, at);
            CoreStorage::flush(&mut storage);
        }
    }

    let stdout = run_python(dir.path(), &["ages"]);
    let python_now = python_now(&stdout);
    let ages = tagged(&stdout, "AGE");
    assert_eq!(ages.len(), remembered_at.len(), "python output: {stdout}");
    for (hash, at) in &remembered_at {
        let age = ages[hash];
        let held = python_now - at.before;
        eprintln!(
            "{} remembered {held:.4} s ago, rnsd reads {age:.4} s",
            hex::encode(hash)
        );
        assert!(
            at.holds_age(python_now, age),
            "{}: rnsd reads an age of {age:.4} s, the identity was remembered \
             between {:.4} and {held:.4} s ago",
            hex::encode(hash),
            python_now - at.after,
        );
    }
}

/// Theirs to ours to theirs: a file `rnsd` wrote with announces an hour, a
/// day and a week old goes through our load, one learned destination, and
/// our flush. Every entry rnsd wrote keeps its first element to the bit, and
/// rnsd reading our file back reports the ages it wrote.
#[test]
fn rnsd_ages_survive_our_load_and_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let written = tagged(
        &run_python(dir.path(), &["write", "3600", "86400", "604800"]),
        "STAMP",
    );
    assert_eq!(written.len(), 3);

    let newcomer;
    let newcomer_at;
    {
        let mut storage = StdStorage::new(dir.path()).expect("storage over rnsd's file");
        (newcomer, newcomer_at) = remember(&mut storage);
        CoreStorage::flush(&mut storage);
    }

    let ours = file_stamps(dir.path());
    for (hash, stamp) in &written {
        assert_eq!(
            ours.get(hash).copied(),
            Some(*stamp),
            "{}: our flush must leave rnsd's announce time alone",
            hex::encode(hash)
        );
    }

    let stdout = run_python(dir.path(), &["ages"]);
    let python_now = python_now(&stdout);
    let ages = tagged(&stdout, "AGE");
    let windows = written
        .iter()
        .map(|(hash, stamp)| {
            let at = Window {
                before: *stamp,
                after: *stamp,
            };
            (*hash, at)
        })
        .chain([(newcomer, newcomer_at)]);
    for (hash, at) in windows {
        let age = ages[&hash];
        assert!(
            at.holds_age(python_now, age),
            "{}: rnsd reads {age:.4} s back, the announce is {:.4} s old",
            hex::encode(hash),
            python_now - at.before,
        );
    }
}
