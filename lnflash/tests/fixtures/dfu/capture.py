#!/usr/bin/env python3
# Regenerate the serial-DFU reference bytes lnflash's dfu-serial transport is
# held to (lnflash/src/dfu.rs, tests `*_matches_adafruit_nrfutil`).
#
# Run with the interpreter adafruit-nrfutil is installed into, from this
# directory:
#
#   ~/.local/share/pipx/venvs/adafruit-nrfutil/bin/python capture.py
#
# It writes four files, all of them outputs of the installed tool's own code:
#
#   app.hex    the input: two spans at 0x27000 with a gap between them and an
#              odd tail, so the 0xFF gap fill and the round-up to a word are
#              exercised, and bytes 0xC0/0xDB in the data, so SLIP escaping is
#   app.bin    what `dfu genpkg` puts in the package as the image
#   app.dat    what `dfu genpkg` puts in the package as the init packet
#   wire.bin   every byte `dfu serial` writes to the port, in order: start,
#              init, data frames, stop
#
# The package is made exactly as scripts/t1000e-dfu-package.sh makes it
# (`--dev-type 0x0052 --sd-req 0x0123`, every other option at its default),
# and sent as `dfu serial --singlebank` sends it, through the tool's own
# `Dfu` and `DfuTransportSerial` classes. Only two things are replaced: the
# pyserial `Serial` class, by a port that records what is written and answers
# every frame with the acknowledgement a bootloader sends
# (hci_transport.c: ack_transmit), and `time.sleep`, so the run takes no
# wall time. Neither changes a byte on the wire.
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile

import nordicsemi
import nordicsemi.dfu.dfu_transport_serial as dts
from nordicsemi.dfu.dfu import Dfu
from nordicsemi.version import NRFUTIL_VERSION

HERE = os.path.dirname(os.path.abspath(__file__))


def hex_record(kind, offset, data):
    body = bytes([len(data), (offset >> 8) & 0xFF, offset & 0xFF, kind]) + data
    return ":" + (body + bytes([(-sum(body)) & 0xFF])).hex().upper() + "\n"


def write_hex(path):
    # 600 bytes at 0x27000, a 168-byte hole, then 430 bytes at 0x272C0. The
    # image ends at 0x2746E, two bytes short of a word.
    spans = [
        (0x27000, bytes(((i * 7) + 0xC0) & 0xFF for i in range(600))),
        (0x272C0, bytes(((i * 13) + 0xDB) & 0xFF for i in range(430))),
    ]
    out = [hex_record(0x04, 0, bytes([0x00, 0x02]))]
    for start, data in spans:
        for at in range(0, len(data), 16):
            out.append(hex_record(0x00, (start + at) & 0xFFFF, data[at:at + 16]))
    out.append(":00000001FF\n")
    with open(path, "w") as f:
        f.writelines(out)


class RecordingPort:
    written = bytearray()

    def __init__(self, *args, **kwargs):
        self.pending = bytearray()

    def write(self, data):
        frame = bytes(data)
        RecordingPort.written += frame
        # The header follows the opening 0xC0 and is never escaped: its first
        # byte always has bits 6 and 7 set and differs from 0xC0 and 0xDB.
        seq = frame[1] & 0x07
        ack = [((seq + 1) % 8) << 3, 0, 0]
        ack.append((~sum(ack) + 1) & 0xFF)
        self.pending += bytes([0xC0] + ack + [0xC0])

    def read(self, n):
        out, self.pending = self.pending[:n], self.pending[n:]
        return bytes(out)

    def setDTR(self, value):
        pass

    def isOpen(self):
        return True

    def close(self):
        pass


def main():
    work = tempfile.mkdtemp(prefix="lnflash-dfu-fixture-")
    try:
        hex_path = os.path.join(HERE, "app.hex")
        write_hex(hex_path)
        shutil.copy(hex_path, os.path.join(work, "app.hex"))
        package = os.path.join(work, "app.zip")
        subprocess.run(
            [sys.executable, "-m", "nordicsemi", "dfu", "genpkg",
             "--dev-type", "0x0052", "--sd-req", "0x0123",
             "--application", os.path.join(work, "app.hex"), package],
            check=True,
        )
        with zipfile.ZipFile(package) as z:
            for name in ("app.bin", "app.dat"):
                with open(os.path.join(HERE, name), "wb") as f:
                    f.write(z.read(name))

        dts.Serial = RecordingPort
        dts.time.sleep = lambda _s: None
        transport = dts.DfuTransportSerial("recording", 115200, False, single_bank=True, touch=0)
        dfu = Dfu(package, dfu_transport=transport)
        import nordicsemi.dfu.dfu as dfu_mod
        dfu_mod.sleep = lambda _s: None
        dfu.dfu_send_images()
        with open(os.path.join(HERE, "wire.bin"), "wb") as f:
            f.write(bytes(RecordingPort.written))
        print("adafruit-nrfutil", NRFUTIL_VERSION, "from", os.path.dirname(nordicsemi.__file__))
    finally:
        shutil.rmtree(work)


if __name__ == "__main__":
    main()
