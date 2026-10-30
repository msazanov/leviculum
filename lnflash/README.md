# lnflash

The flasher for LNode boards. It finds the attached boards, brings each into
its bootloader, confirms from the bootloader what the board actually is,
checks the SoftDevice precondition, and only then writes the leviculum
firmware, over UF2 mass storage or serial DFU. Once a board is back up it
offers the radio settings and the telemetry choice.

It flashes from a bundle: one image per supported board, and Nordic's S140
SoftDevice with its licence for the board that can need it. The nightly
publishes that bundle twice. As `lnflash-nightly-amd64.tar.gz`, unpacked and
run in place with `sudo ./lnflash`; and as the `lnflash` package, which puts
the binary in `/usr/bin` and the bundle in `/usr/share/lnflash`, the last
place lnflash looks for one, so `sudo lnflash` works from any directory.

Root is needed: the bootloader's drive is a root:disk block device. Add
`--dry-run` to see what is attached and what would happen, without changing
anything. Every option is in `lnflash(1)`; the design and the evidence behind
its constants are in `docs/src/concepts/lnode-flashing.md`.
