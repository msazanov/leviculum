//! The **transport** axis, third kind: the ESP32 ROM's serial loader,
//! spoken through an external flasher.
//!
//! An ESP32-class chip has no UF2 drive and no Nordic DFU. Its mask ROM holds
//! a serial loader that a USB-UART bridge reaches by toggling DTR and RTS
//! (EN low, GPIO0 low, EN high), and every image goes in through it. The
//! ThinkNode M9 is the first LNode on this path (Codeberg #473): an
//! ESP32-S3 behind a CH340K, `1a86:7522`, with the chip's own USB not on the
//! connector (docs/src/concepts/thinknode-m9.md).
//!
//! **This module runs a program, which the rest of lnflash does not.** The
//! UF2 and DFU transports are syscalls, because the bundle promises "no
//! external programs" (`lib.rs`). The ROM loader protocol plus the RAM stub
//! that makes it fast is a larger piece of work, and the two tools that
//! already speak it are on every bench that builds ESP firmware. So this
//! transport composes their command lines, runs them through a [`Runner`],
//! and judges their exit status. A native implementation can replace the
//! runner later without moving the catalogue or the flow.
//!
//! Two flashers, in this order:
//!
//! 1. **`espflash`**, which carries its own RAM stubs for every chip
//!    (`cargo install espflash`, scripts/install-ci.sh step 6c). It is the
//!    only one that takes an ELF.
//! 2. **`esptool`**, with `--no-stub` unless its package holds the stub for
//!    this chip. Debian's esptool 4.7.0 is stripped of every Xtensa stub
//!    (esp32, esp32s2, esp32s3) and dies with `FileNotFoundError:
//!    .../stub_flasher_32s3.json` without the flag; the pinned PyPI build
//!    scripts/install-esptool.sh installs carries them. The stub question is
//!    asked of the Python that would run that esptool, never assumed.
//!
//! The command lines use esptool's underscore spellings (`write_flash`,
//! `--before default_reset`): esptool 5 still takes them with a deprecation
//! warning, esptool 4 takes nothing else, so one form drives both.
//!
//! **The line speed is the board's, not the tool's.** The CH340K on the M9
//! loses bytes above 230400 baud: espflash reported `corrupt_data` at 460800
//! and 921600, and 230400 read and wrote the board on 2026-10-25 (bench notes
//! for #473). The rate is therefore a catalogue fact, `[board.<name>.esp]`
//! `baud`, carried here in [`Params`].

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// The chips this transport knows the flash layout of. The names are the
/// `--chip` values both flashers accept.
pub const CHIPS: &[&str] = &[
    "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c6", "esp32h2",
];

/// The rates both flashers and the ROM loader agree on.
pub const BAUDS: &[u32] = &[115_200, 230_400, 460_800, 921_600];

/// Where the partition table sits in every layout ESP-IDF produces.
pub const PARTITION_TABLE_OFFSET: u32 = 0x8000;

/// Where the first application partition starts in the layouts we build
/// (`factory` at `0x10000`, leviculum-esp's `partitions.csv`).
pub const APP_OFFSET: u32 = 0x1_0000;

/// Where the second-stage bootloader goes. The two oldest Xtensa parts keep
/// it one sector in, every later chip at zero; the RNode recipes carry the
/// same table (`just rnode-chip-offsets`).
pub fn bootloader_offset(chip: &str) -> u32 {
    match chip {
        "esp32" | "esp32s2" => 0x1000,
        _ => 0x0,
    }
}

/// What a write needs to know about the chip and the line to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// `--chip`, one of [`CHIPS`].
    pub chip: String,
    /// The line speed, one of [`BAUDS`].
    pub baud: u32,
}

/// The images one write puts on the chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Images {
    /// A linked ELF. espflash builds the application image from it and
    /// writes its own bootloader, the given partition table (or its
    /// default), and the application. Only espflash can do this.
    Elf {
        elf: PathBuf,
        partition_table: Option<PathBuf>,
    },
    /// Binary images at fixed addresses: a bootloader, a partition table and
    /// an application, or one merged image at zero.
    Segments(Vec<(u32, PathBuf)>),
}

impl Images {
    /// Bootloader, partition table and application at the addresses
    /// [`bootloader_offset`], [`PARTITION_TABLE_OFFSET`] and [`APP_OFFSET`]
    /// name for `chip`.
    pub fn parts(chip: &str, bootloader: PathBuf, partition_table: PathBuf, app: PathBuf) -> Self {
        Images::Segments(vec![
            (bootloader_offset(chip), bootloader),
            (PARTITION_TABLE_OFFSET, partition_table),
            (APP_OFFSET, app),
        ])
    }

    /// One image covering the flash from address zero, as `espflash
    /// save-image --merge` or `esptool merge_bin` writes it.
    pub fn merged(image: PathBuf) -> Self {
        Images::Segments(vec![(0, image)])
    }
}

/// The program that does the talking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flasher {
    Espflash {
        program: PathBuf,
    },
    /// `stub` is whether this esptool's package carries the RAM stub for
    /// the chip; without it every command gets `--no-stub`.
    Esptool {
        program: PathBuf,
        stub: bool,
    },
}

impl Flasher {
    pub fn describe(&self) -> String {
        match self {
            Flasher::Espflash { program } => format!("espflash ({})", program.display()),
            Flasher::Esptool {
                program,
                stub: true,
            } => format!("esptool ({}) with its stub", program.display()),
            Flasher::Esptool {
                program,
                stub: false,
            } => format!(
                "esptool ({}) without a stub for this chip, so --no-stub",
                program.display()
            ),
        }
    }
}

/// One program invocation, not yet run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl Command {
    fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program.display())?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        Ok(())
    }
}

/// Where commands go. A trait so the composition and the order are
/// testable without a board or a flasher on the test host.
pub trait Runner {
    /// Run to completion with the terminal attached, so the flasher's own
    /// progress reaches the person. `Ok(true)` is exit status zero; `Err`
    /// only when the program could not be started at all.
    fn run(&mut self, command: &Command) -> io::Result<bool>;

    /// Run silently and say whether it succeeded. A program that cannot be
    /// started is a `false`, which is exactly what a search wants to know.
    fn probe(&mut self, command: &Command) -> bool;
}

/// The runner that starts real processes.
pub struct System;

impl Runner for System {
    fn run(&mut self, command: &Command) -> io::Result<bool> {
        std::process::Command::new(&command.program)
            .args(&command.args)
            .status()
            .map(|status| status.success())
    }

    fn probe(&mut self, command: &Command) -> bool {
        std::process::Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

/// Where [`find_flasher`] looks besides `PATH`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// The home whose `.cargo/bin` holds a `cargo install`ed espflash.
    pub home: Option<PathBuf>,
    /// The venv scripts/install-esptool.sh installs into.
    pub esptool_venv: Option<PathBuf>,
}

impl Search {
    /// From the environment: `$HOME`, and the venv at
    /// `$LEVICULUM_RNODE_TOOLS` or `$HOME/.rnode-tools/venv`, the same
    /// resolution scripts/install-esptool.sh and the Justfile use.
    ///
    /// Under `sudo` `$HOME` is root's, so a flasher installed for the user
    /// is found only through `PATH`.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let esptool_venv = std::env::var_os("LEVICULUM_RNODE_TOOLS")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".rnode-tools").join("venv")));
        Self { home, esptool_venv }
    }
}

/// The Python a stub check runs: exit 0 iff the importable esptool package
/// holds a stub for the chip in `argv[1]`. esptool 4 names it
/// `stub_flasher_32s3.json`, esptool 5 `esp32s3.json` under a numbered
/// directory; both live below `targets/stub_flasher`.
const STUB_CHECK: &str = "import esptool, os, sys
chip = sys.argv[1]
want = {'stub_flasher_' + chip[3:] + '.json', chip + '.json'}
root = os.path.join(os.path.dirname(esptool.__file__), 'targets', 'stub_flasher')
sys.exit(0 if any(want & set(files) for _, _, files in os.walk(root)) else 1)";

/// Find a flasher that can talk to `chip`, espflash first.
///
/// Every question goes through the runner, so the search is the same
/// sequence on a test host as on a bench: `--version` (or esptool's
/// `version`) answers "is it there", and for an esptool a stub check run by
/// the Python beside it answers "does it carry the stub".
pub fn find_flasher(runner: &mut dyn Runner, search: &Search, chip: &str) -> Option<Flasher> {
    let mut espflash = vec![PathBuf::from("espflash")];
    if let Some(home) = &search.home {
        espflash.push(home.join(".cargo").join("bin").join("espflash"));
    }
    for program in espflash {
        if runner.probe(&Command::new(&program).arg("--version")) {
            return Some(Flasher::Espflash { program });
        }
    }

    // The pinned venv first: it is the one a bench installs on purpose, and
    // its own Python is the one that imports its esptool.
    let mut esptool = Vec::new();
    if let Some(venv) = &search.esptool_venv {
        let bin = venv.join("bin");
        esptool.push((bin.join("esptool"), bin.join("python")));
    }
    esptool.push((PathBuf::from("esptool"), PathBuf::from("python3")));
    for (program, python) in esptool {
        if runner.probe(&Command::new(&program).arg("version")) {
            let stub = runner.probe(&Command::new(&python).arg("-c").arg(STUB_CHECK).arg(chip));
            return Some(Flasher::Esptool { program, stub });
        }
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "no flasher for {chip} on this host: install espflash (cargo install espflash) or \
         esptool (scripts/install-esptool.sh)"
    )]
    NoFlasher { chip: String },
    #[error("{flasher} cannot write an ELF; only espflash builds the image from one")]
    ElfNeedsEspflash { flasher: String },
    #[error("nothing to write: the image set is empty")]
    Empty,
    #[error("chip {chip:?} is not one this transport knows ({})", CHIPS.join(", "))]
    UnknownChip { chip: String },
    #[error("{baud} baud is not a rate the ROM loader and the flashers share")]
    BadBaud { baud: u32 },
    #[error("could not start `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("step {step} of {of} failed: `{command}`")]
    Failed {
        step: usize,
        of: usize,
        command: String,
    },
}

/// The commands that write `images` through `flasher` on `port`, in the
/// order they run. Pure, so the composition is what the tests hold.
pub fn commands(
    flasher: &Flasher,
    params: &Params,
    port: &Path,
    images: &Images,
) -> Result<Vec<Command>, Error> {
    if !CHIPS.contains(&params.chip.as_str()) {
        return Err(Error::UnknownChip {
            chip: params.chip.clone(),
        });
    }
    if !BAUDS.contains(&params.baud) {
        return Err(Error::BadBaud { baud: params.baud });
    }
    if let Images::Segments(segments) = images {
        if segments.is_empty() {
            return Err(Error::Empty);
        }
    }
    let baud = params.baud.to_string();
    match (flasher, images) {
        (
            Flasher::Espflash { program },
            Images::Elf {
                elf,
                partition_table,
            },
        ) => {
            let mut command = Command::new(program)
                .arg("flash")
                .arg("--chip")
                .arg(&params.chip)
                .arg("--port")
                .arg(port)
                .arg("--baud")
                .arg(&baud)
                .arg("--non-interactive");
            if let Some(table) = partition_table {
                command = command.arg("--partition-table").arg(table);
            }
            Ok(vec![command.arg(elf)])
        }
        // espflash writes one file per `write-bin`, and each run resets the
        // chip into the loader on its own, so a segment list is a sequence.
        // No `--non-interactive`: on `write-bin` it is a monitor option, and
        // espflash 4.6.0 warns that it ignores it.
        (Flasher::Espflash { program }, Images::Segments(segments)) => Ok(segments
            .iter()
            .map(|(address, file)| {
                Command::new(program)
                    .arg("write-bin")
                    .arg("--chip")
                    .arg(&params.chip)
                    .arg("--port")
                    .arg(port)
                    .arg("--baud")
                    .arg(&baud)
                    .arg(format!("{address:#x}"))
                    .arg(file)
            })
            .collect()),
        (Flasher::Esptool { .. }, Images::Elf { .. }) => Err(Error::ElfNeedsEspflash {
            flasher: flasher.describe(),
        }),
        (Flasher::Esptool { program, stub }, Images::Segments(segments)) => {
            let mut command = Command::new(program)
                .arg("--chip")
                .arg(&params.chip)
                .arg("--port")
                .arg(port)
                .arg("--baud")
                .arg(&baud)
                .arg("--before")
                .arg("default_reset")
                .arg("--after")
                .arg("hard_reset");
            if !stub {
                command = command.arg("--no-stub");
            }
            command = command.arg("write_flash");
            for (address, file) in segments {
                command = command.arg(format!("{address:#x}")).arg(file);
            }
            Ok(vec![command])
        }
    }
}

/// Write `images` to the chip on `port`, stopping at the first command that
/// fails. A failure names the step, because a segment sequence that stopped
/// after the bootloader is a different board state from one that never
/// started.
pub fn flash(
    runner: &mut dyn Runner,
    flasher: &Flasher,
    params: &Params,
    port: &Path,
    images: &Images,
) -> Result<(), Error> {
    let commands = commands(flasher, params, port, images)?;
    let of = commands.len();
    for (index, command) in commands.iter().enumerate() {
        let ok = runner.run(command).map_err(|source| Error::Spawn {
            command: command.to_string(),
            source,
        })?;
        if !ok {
            return Err(Error::Failed {
                step: index + 1,
                of,
                command: command.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every command and answers from a script: a probe succeeds
    /// iff its rendered command line is in `present`, a run fails iff its
    /// index is `fail_at`.
    #[derive(Default)]
    struct Fake {
        present: Vec<String>,
        fail_at: Option<usize>,
        probed: Vec<String>,
        ran: Vec<String>,
    }

    impl Runner for Fake {
        fn run(&mut self, command: &Command) -> io::Result<bool> {
            let index = self.ran.len();
            self.ran.push(command.to_string());
            Ok(self.fail_at != Some(index))
        }

        fn probe(&mut self, command: &Command) -> bool {
            let line = command.to_string();
            self.probed.push(line.clone());
            self.present.iter().any(|p| line.starts_with(p.as_str()))
        }
    }

    fn m9() -> Params {
        Params {
            chip: "esp32s3".into(),
            baud: 230_400,
        }
    }

    fn port() -> PathBuf {
        PathBuf::from("/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0")
    }

    fn search() -> Search {
        Search {
            home: Some(PathBuf::from("/home/u")),
            esptool_venv: Some(PathBuf::from("/home/u/.rnode-tools/venv")),
        }
    }

    #[test]
    fn espflash_writes_an_elf_the_way_the_m9_was_flashed_on_the_bench() {
        // The command that wrote the rig's M9 on 2026-10-25, 71.6 s, rc=0:
        // espflash flash --chip esp32s3 -p <port> --non-interactive
        //   --baud 230400 --partition-table partitions.csv <elf>
        let flasher = Flasher::Espflash {
            program: "espflash".into(),
        };
        let images = Images::Elf {
            elf: "thinknode_m9".into(),
            partition_table: Some("partitions.csv".into()),
        };
        let mut fake = Fake::default();
        flash(&mut fake, &flasher, &m9(), &port(), &images).unwrap();
        assert_eq!(
            fake.ran,
            vec![
                "espflash flash --chip esp32s3 --port \
                 /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 --baud 230400 \
                 --non-interactive --partition-table partitions.csv thinknode_m9"
            ]
        );
    }

    #[test]
    fn esptool_without_the_stub_writes_all_three_parts_in_one_run() {
        let flasher = Flasher::Esptool {
            program: "/usr/bin/esptool".into(),
            stub: false,
        };
        let images = Images::parts(
            "esp32s3",
            "bootloader.bin".into(),
            "partitions.bin".into(),
            "app.bin".into(),
        );
        let mut fake = Fake::default();
        flash(&mut fake, &flasher, &m9(), &port(), &images).unwrap();
        assert_eq!(
            fake.ran,
            vec![
                "/usr/bin/esptool --chip esp32s3 --port \
                 /dev/serial/by-id/usb-1a86_USB_Serial-if00-port0 --baud 230400 \
                 --before default_reset --after hard_reset --no-stub write_flash \
                 0x0 bootloader.bin 0x8000 partitions.bin 0x10000 app.bin"
            ]
        );
    }

    #[test]
    fn esptool_with_the_stub_is_not_slowed_to_the_rom_loader() {
        let flasher = Flasher::Esptool {
            program: "esptool".into(),
            stub: true,
        };
        let commands = commands(
            &flasher,
            &m9(),
            &port(),
            &Images::merged("merged.bin".into()),
        )
        .unwrap();
        assert_eq!(commands.len(), 1);
        let line = commands[0].to_string();
        assert!(!line.contains("--no-stub"), "{line}");
        assert!(line.ends_with("write_flash 0x0 merged.bin"), "{line}");
    }

    #[test]
    fn the_bootloader_address_follows_the_chip() {
        assert_eq!(bootloader_offset("esp32s3"), 0x0);
        assert_eq!(bootloader_offset("esp32c3"), 0x0);
        assert_eq!(bootloader_offset("esp32"), 0x1000);
        assert_eq!(bootloader_offset("esp32s2"), 0x1000);
    }

    #[test]
    fn espflash_writes_segments_one_run_each_and_stops_at_the_first_failure() {
        let flasher = Flasher::Espflash {
            program: "espflash".into(),
        };
        let images = Images::parts(
            "esp32s3",
            "bootloader.bin".into(),
            "partitions.bin".into(),
            "app.bin".into(),
        );
        let mut fake = Fake {
            fail_at: Some(1),
            ..Fake::default()
        };
        let err = flash(&mut fake, &flasher, &m9(), &port(), &images).unwrap_err();
        assert!(
            matches!(err, Error::Failed { step: 2, of: 3, .. }),
            "the partition table failed, so the bootloader is on the chip and the app is not: \
             {err:?}"
        );
        assert_eq!(fake.ran.len(), 2, "nothing runs after the failure");
        assert!(fake.ran[0].contains("write-bin"));
        assert!(fake.ran[0].ends_with("0x0 bootloader.bin"));
        assert!(fake.ran[1].ends_with("0x8000 partitions.bin"));
    }

    #[test]
    fn an_elf_through_esptool_is_refused_before_anything_runs() {
        let flasher = Flasher::Esptool {
            program: "esptool".into(),
            stub: true,
        };
        let images = Images::Elf {
            elf: "thinknode_m9".into(),
            partition_table: None,
        };
        let mut fake = Fake::default();
        let err = flash(&mut fake, &flasher, &m9(), &port(), &images).unwrap_err();
        assert!(matches!(err, Error::ElfNeedsEspflash { .. }), "{err:?}");
        assert!(fake.ran.is_empty());
    }

    #[test]
    fn a_rate_the_loader_does_not_share_and_an_unknown_chip_are_refused() {
        let flasher = Flasher::Espflash {
            program: "espflash".into(),
        };
        let images = Images::merged("m.bin".into());
        let fast = Params {
            baud: 250_000,
            ..m9()
        };
        assert!(matches!(
            commands(&flasher, &fast, &port(), &images),
            Err(Error::BadBaud { baud: 250_000 })
        ));
        let nrf = Params {
            chip: "nrf52840".into(),
            ..m9()
        };
        assert!(matches!(
            commands(&flasher, &nrf, &port(), &images),
            Err(Error::UnknownChip { .. })
        ));
        assert!(matches!(
            commands(&flasher, &m9(), &port(), &Images::Segments(Vec::new())),
            Err(Error::Empty)
        ));
    }

    #[test]
    fn espflash_is_preferred_and_found_in_cargo_bin_when_not_on_path() {
        // The rig host: espflash only in ~/.cargo/bin, which is not on the
        // PATH of a non-login shell, and Debian's esptool on PATH.
        let mut fake = Fake {
            present: vec![
                "/home/u/.cargo/bin/espflash --version".into(),
                "esptool version".into(),
            ],
            ..Fake::default()
        };
        let found = find_flasher(&mut fake, &search(), "esp32s3");
        assert_eq!(
            found,
            Some(Flasher::Espflash {
                program: "/home/u/.cargo/bin/espflash".into()
            })
        );
        assert_eq!(
            fake.probed,
            vec![
                "espflash --version",
                "/home/u/.cargo/bin/espflash --version"
            ],
            "the search stops at the first flasher it finds"
        );
    }

    #[test]
    fn debians_esptool_is_found_without_the_stub_and_the_venv_one_with_it() {
        // Debian's: on PATH, its python3 finds no 32s3 stub.
        let mut fake = Fake {
            present: vec!["esptool version".into()],
            ..Fake::default()
        };
        assert_eq!(
            find_flasher(&mut fake, &search(), "esp32s3"),
            Some(Flasher::Esptool {
                program: "esptool".into(),
                stub: false
            })
        );
        let check = fake.probed.last().unwrap();
        assert!(check.starts_with("python3 -c import esptool"), "{check}");
        assert!(check.ends_with(" esp32s3"), "{check}");

        // The pinned venv: preferred over PATH, and asked through its own
        // Python, which is the one that imports it.
        let mut fake = Fake {
            present: vec![
                "/home/u/.rnode-tools/venv/bin/esptool version".into(),
                "/home/u/.rnode-tools/venv/bin/python -c".into(),
                "esptool version".into(),
            ],
            ..Fake::default()
        };
        assert_eq!(
            find_flasher(&mut fake, &search(), "esp32s3"),
            Some(Flasher::Esptool {
                program: "/home/u/.rnode-tools/venv/bin/esptool".into(),
                stub: true
            })
        );
    }

    #[test]
    fn a_host_with_no_flasher_finds_none() {
        let mut fake = Fake::default();
        assert_eq!(find_flasher(&mut fake, &search(), "esp32s3"), None);
        assert_eq!(fake.probed.len(), 4, "{:?}", fake.probed);
    }

    #[test]
    fn the_stub_check_names_both_esptool_generations_stub_files() {
        // esptool 4.7.0 (Debian): stub_flasher/stub_flasher_32s3.json.
        // esptool 5.4.0 (PyPI): stub_flasher/2/esp32s3.json.
        assert!(STUB_CHECK.contains("'stub_flasher_' + chip[3:] + '.json'"));
        assert!(STUB_CHECK.contains("chip + '.json'"));
        assert!(STUB_CHECK.contains("os.walk(root)"));
    }
}
