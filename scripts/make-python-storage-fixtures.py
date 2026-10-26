#!/usr/bin/env python3
"""Write the Python-RNS storage fixtures the leviculum-std compat tests read.

The three `test_python_*_compat` lib tests in `leviculum-std` check that our
decoders read the files the reference rnsd writes under `<configdir>/storage`.
They used to read `$HOME/.reticulum/storage` of whatever rnsd the host had,
which made them vacuous without one and red when its history was empty. This
script produces the files once, from the reference itself, with no host state:

    python3 scripts/make-python-storage-fixtures.py \\
        leviculum-std/tests/fixtures/python-storage

It starts two throwaway Reticulum instances in TempDir config directories,
joined over loopback TCP. The announcer announces one destination; the
receiver (transport enabled, or `save_packet_hashlist` writes an empty list,
`reference/Reticulum/RNS/Transport.py:3214`) learns it, then shuts down
through the reference's own exit handler (`Reticulum.exit_handler`,
`reference/Reticulum/RNS/Reticulum.py:179`), which persists
`packet_hashlist` (`Transport.py:3218`) and `known_destinations`
(`Identity.py:220`). `transport_identity` is written at start
(`Transport.py:225`). The three files are copied into the output directory.
"""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_RNS_PATH = os.environ.get("RETICULUM_PATH") or os.path.join(
    _ROOT, "reference/Reticulum"
)
if os.path.isdir(os.path.join(_RNS_PATH, "RNS")):
    sys.path.insert(0, _RNS_PATH)

FILES = ("packet_hashlist", "known_destinations", "transport_identity")
APP_NAME = "leviculum_fixture"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def write_config(configdir, transport, interface):
    os.makedirs(configdir, exist_ok=True)
    with open(os.path.join(configdir, "config"), "w") as f:
        f.write(
            "[reticulum]\n"
            f"  enable_transport = {'Yes' if transport else 'No'}\n"
            "  share_instance = No\n"
            "  panic_on_interface_error = Yes\n"
            "\n[logging]\n  loglevel = 2\n"
            "\n[interfaces]\n" + interface
        )


def announcer(configdir):
    import RNS

    RNS.Reticulum(configdir)
    identity = RNS.Identity()
    dest = RNS.Destination(
        identity, RNS.Destination.IN, RNS.Destination.SINGLE, APP_NAME, "announce"
    )
    # The reference logs to stdout too, so the hash carries a marker.
    print("DEST " + dest.hash.hex(), flush=True)
    # Announce until the receiver has it and closes our stdin.
    while True:
        dest.announce(app_data=b"leviculum fixture")
        time.sleep(1)


def receiver(configdir, dest_hex):
    import RNS

    reticulum = RNS.Reticulum(configdir)
    dest_hash = bytes.fromhex(dest_hex)
    deadline = time.time() + 30
    while not RNS.Transport.has_path(dest_hash):
        if time.time() > deadline:
            print("ERROR: no announce received within 30 s", file=sys.stderr)
            return 1
        time.sleep(0.2)
    if RNS.Identity.recall(dest_hash) is None:
        print("ERROR: path known but identity not recalled", file=sys.stderr)
        return 1
    RNS.Reticulum.exit_handler()
    del reticulum
    return 0


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--announcer":
        return announcer(sys.argv[2])
    if len(sys.argv) == 4 and sys.argv[1] == "--receiver":
        return receiver(sys.argv[2], sys.argv[3])
    if len(sys.argv) != 2:
        print(__doc__)
        return 2

    outdir = os.path.abspath(sys.argv[1])
    port = free_port()
    with tempfile.TemporaryDirectory(prefix="lev-pyfixture-") as tmp:
        a_dir = os.path.join(tmp, "announcer")
        r_dir = os.path.join(tmp, "receiver")
        write_config(
            a_dir,
            False,
            "  [[Fixture Server]]\n    type = TCPServerInterface\n    enabled = yes\n"
            f"    listen_ip = 127.0.0.1\n    listen_port = {port}\n",
        )
        write_config(
            r_dir,
            True,
            "  [[Fixture Client]]\n    type = TCPClientInterface\n    enabled = yes\n"
            f"    target_host = 127.0.0.1\n    target_port = {port}\n",
        )
        ann = subprocess.Popen(
            [sys.executable, __file__, "--announcer", a_dir],
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            dest_hex = None
            for line in ann.stdout:
                if line.startswith("DEST "):
                    dest_hex = line.split()[1]
                    break
            if not dest_hex:
                print("ERROR: announcer printed no hash", file=sys.stderr)
                return 1
            rc = subprocess.call(
                [sys.executable, __file__, "--receiver", r_dir, dest_hex],
                timeout=60,
            )
            if rc != 0:
                return rc
        finally:
            ann.kill()
            ann.wait()

        os.makedirs(outdir, exist_ok=True)
        storage = os.path.join(r_dir, "storage")
        for name in FILES:
            src = os.path.join(storage, name)
            if not os.path.isfile(src) or os.path.getsize(src) == 0:
                print(f"ERROR: reference did not write {name}", file=sys.stderr)
                return 1
            shutil.copyfile(src, os.path.join(outdir, name))
            print(f"{name} {os.path.getsize(src)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
