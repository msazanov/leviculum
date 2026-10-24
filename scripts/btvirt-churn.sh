#!/usr/bin/env bash
# Timer-fd churn bench for the installed btvirt (order 955).
#
# btvirt -s serves one emulated LE controller per client on the unix socket
# /tmp/bt-server-le, raw H4, private to the process: no vhci, no host hci
# device, no bluetoothd. So this runs beside other work without the rig or the
# docker lock.
#
# What it drives:
#   - five LOAD clients, each a 20 ms legacy advertiser (Set Advertising
#     Parameters 0x2006 with Advertising_Interval_Min 0x0020, then Set
#     Advertising Enable 0x200A 0x01) plus an active scan, so the
#     adv-every-interval patch keeps five broadcast timer fds firing and the
#     scanners receive the reports;
#   - one CHURN client that, every CADENCE ms for WINDOW s, sends HCI Reset
#     (0x0C03), then 0x2006 and 0x200A 0x01 again. The reset leaves the
#     broadcast timer to remove itself at its next expiry and the enable
#     removes and re-adds one, so timer fds come and go inside the same event
#     batch as the five steady ones.
#
# At the end of each window it checks that the btvirt child is still alive and
# records how it exited if not. Then it counts the kernel log lines naming
# btvirt since the first run started (a segfault or abort leaves one line per
# termination) and writes that raw kernel output to
# scripts/.btvirt-churn-kern.log, never to stdout.
#
# Exit status: 0 when every run kept btvirt alive and the kernel log names it
# nowhere, 1 when btvirt died or the kernel log names it, 2 on a setup error.
#
# Usage:
#   scripts/btvirt-churn.sh [--window S] [--cadence-ms MS] [--runs N]
#
# Environment (argv wins):
#   CHURN_WINDOW      seconds per run (default 30)
#   CHURN_CADENCE_MS  churn loop period in ms (default 20)
#   CHURN_RUNS        number of runs, each on a fresh btvirt (default 1)
#   BTVIRT            emulator binary (default /usr/local/bin/btvirt)
#
# It refuses to start beside another btvirt: -s unlinks and rebinds the
# /tmp/bt-server-* sockets, which would take them from whoever holds them.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export CHURN_KERN_LOG="$ROOT/scripts/.btvirt-churn-kern.log"

exec python3 - "$@" <<'PY'
import argparse
import os
import selectors
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

SOCK_PATH = "/tmp/bt-server-le"
LOAD_CLIENTS = 5

OP_RESET = 0x0C03
OP_LE_SET_ADV_PARAMS = 0x2006
OP_LE_SET_ADV_ENABLE = 0x200A
OP_LE_SET_SCAN_PARAMS = 0x200B
OP_LE_SET_SCAN_ENABLE = 0x200C

# Advertising_Interval_Min/Max 0x0020 = 20 ms, ADV_IND, public address,
# all three channels, no filter.
ADV_PARAMS = struct.pack("<HHBBB6sBB", 0x0020, 0x0020, 0x00, 0x00, 0x00,
                         b"\x00" * 6, 0x07, 0x00)
# Active scan, interval = window = 0x0010 (10 ms), public address, no filter.
SCAN_PARAMS = struct.pack("<BHHBB", 0x01, 0x0010, 0x0010, 0x00, 0x00)


def hci_cmd(opcode, params=b""):
    return struct.pack("<BHB", 0x01, opcode, len(params)) + params


def parse_args():
    p = argparse.ArgumentParser(description="btvirt timer-fd churn bench")
    p.add_argument("--window", type=float,
                   default=float(os.environ.get("CHURN_WINDOW", "30")))
    p.add_argument("--cadence-ms", type=float,
                   default=float(os.environ.get("CHURN_CADENCE_MS", "20")))
    p.add_argument("--runs", type=int,
                   default=int(os.environ.get("CHURN_RUNS", "1")))
    p.add_argument("--btvirt",
                   default=os.environ.get("BTVIRT", "/usr/local/bin/btvirt"))
    return p.parse_args()


def describe_status(rc):
    if rc is None:
        return "alive"
    if rc < 0:
        try:
            name = signal.Signals(-rc).name
        except ValueError:
            name = "?"
        return f"killed by signal {-rc} ({name})"
    return f"exited with status {rc}"


class Drain(threading.Thread):
    """Reads and discards every client's events, so btvirt never blocks or
    fails writing to a full socket buffer."""

    def __init__(self):
        super().__init__(daemon=True)
        self.sel = selectors.DefaultSelector()
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.bytes = 0
        self.closed = 0

    def add(self, sock):
        with self.lock:
            self.sel.register(sock, selectors.EVENT_READ)

    def run(self):
        while not self.stop.is_set():
            with self.lock:
                if not self.sel.get_map():
                    ready = []
                else:
                    ready = self.sel.select(timeout=0)
            if not ready:
                time.sleep(0.002)
                continue
            for key, _ in ready:
                try:
                    data = key.fileobj.recv(65536)
                except BlockingIOError:
                    continue
                except OSError:
                    data = b""
                if not data:
                    with self.lock:
                        self.sel.unregister(key.fileobj)
                    self.closed += 1
                else:
                    self.bytes += len(data)


def connect(proc):
    deadline = time.monotonic() + 5
    while True:
        if proc.poll() is not None:
            raise RuntimeError("btvirt exited before its socket accepted: "
                               + describe_status(proc.returncode))
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            s.connect(SOCK_PATH)
            s.setblocking(False)
            return s
        except OSError:
            s.close()
            if time.monotonic() > deadline:
                raise
            time.sleep(0.02)


def send(sock, data):
    # Small packets into a drained socket: one send each, so btvirt's H4
    # parser never sees a command split across reads.
    sock.setblocking(True)
    try:
        sock.sendall(data)
    finally:
        sock.setblocking(False)


def one_run(args, run_no, btvirt_log):
    proc = subprocess.Popen([args.btvirt, "-s"], stdin=subprocess.DEVNULL,
                            stdout=btvirt_log, stderr=subprocess.STDOUT)
    drain = Drain()
    socks = []
    iterations = 0
    send_error = None
    started = time.monotonic()
    try:
        drain.start()
        for _ in range(LOAD_CLIENTS):
            s = connect(proc)
            socks.append(s)
            drain.add(s)
            send(s, hci_cmd(OP_RESET))
            send(s, hci_cmd(OP_LE_SET_ADV_PARAMS, ADV_PARAMS))
            send(s, hci_cmd(OP_LE_SET_ADV_ENABLE, b"\x01"))
            send(s, hci_cmd(OP_LE_SET_SCAN_PARAMS, SCAN_PARAMS))
            send(s, hci_cmd(OP_LE_SET_SCAN_ENABLE, b"\x01\x00"))

        churn = connect(proc)
        socks.append(churn)
        drain.add(churn)
        churn_batch = (hci_cmd(OP_RESET)
                       + hci_cmd(OP_LE_SET_ADV_PARAMS, ADV_PARAMS)
                       + hci_cmd(OP_LE_SET_ADV_ENABLE, b"\x01"))
        period = args.cadence_ms / 1000.0
        started = time.monotonic()
        end = started + args.window
        next_at = started
        while time.monotonic() < end:
            if proc.poll() is not None:
                break
            try:
                send(churn, churn_batch)
            except OSError as e:
                send_error = repr(e)
                break
            iterations += 1
            next_at += period
            delay = next_at - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            else:
                next_at = time.monotonic()
        elapsed = time.monotonic() - started
        # Give a crash in the last batch a moment to surface.
        time.sleep(0.2)
        rc = proc.poll()
    finally:
        drain.stop.set()
        for s in socks:
            s.close()
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
    unexpected = rc is not None
    print(f"BTVIRT_CHURN_RUN run={run_no} window_s={elapsed:.1f} "
          f"iterations={iterations} unexpected_exit={int(unexpected)} "
          f"status=\"{describe_status(rc)}\" event_bytes={drain.bytes}"
          + (f" send_error=\"{send_error}\"" if send_error else ""),
          flush=True)
    return iterations, unexpected


def kernel_lines(since_epoch):
    cmd = ["journalctl", "-k", "--no-pager", "-o", "short-iso",
           "--since", f"@{int(since_epoch)}"]
    for prefix in (["sudo", "-n"], []):
        r = subprocess.run(prefix + cmd, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True)
        if r.returncode == 0:
            return r.stdout, " ".join(prefix + cmd)
    return None, r.stderr.strip()


def main():
    args = parse_args()
    if args.window <= 0 or args.cadence_ms <= 0 or args.runs < 1:
        print("window, cadence-ms and runs must be positive", file=sys.stderr)
        return 2
    if not os.access(args.btvirt, os.X_OK):
        print(f"no executable btvirt at {args.btvirt}", file=sys.stderr)
        return 2
    other = subprocess.run(["pgrep", "-x", "btvirt"], stdout=subprocess.PIPE,
                           text=True).stdout.split()
    if other:
        print(f"another btvirt is running (pid {' '.join(other)}); -s would "
              f"take its sockets, refusing", file=sys.stderr)
        return 2

    # SIGTERM/SIGINT unwind through the finally blocks, so the child and the
    # clients go with us.
    def bail(signum, _frame):
        raise KeyboardInterrupt(signum)
    signal.signal(signal.SIGTERM, bail)

    since = time.time() - 1
    total_iter = 0
    total_exits = 0
    with tempfile.NamedTemporaryFile(prefix="btvirt-churn-", suffix=".out",
                                     delete=False) as btvirt_log:
        try:
            for run_no in range(1, args.runs + 1):
                n, died = one_run(args, run_no, btvirt_log)
                total_iter += n
                total_exits += int(died)
        except KeyboardInterrupt:
            print("interrupted", file=sys.stderr)
            return 2
        except Exception as e:  # noqa: BLE001 - report any setup failure
            print(f"setup error: {e!r}", file=sys.stderr)
            return 2

    time.sleep(1)  # let journald take the kernel's last lines
    kern, how = kernel_lines(since)
    kern_log = os.environ["CHURN_KERN_LOG"]
    if kern is None:
        print(f"journalctl -k unreadable: {how}", file=sys.stderr)
        kern_count = "unknown"
    else:
        with open(kern_log, "w") as f:
            f.write(f"# {how}\n")
            f.write(kern)
        kern_count = sum(1 for line in kern.splitlines() if "btvirt" in line)

    rate = total_exits / total_iter if total_iter else 0.0
    print(f"BTVIRT_CHURN_TOTAL runs={args.runs} iterations={total_iter} "
          f"unexpected_exits={total_exits} rate={rate:.6g} "
          f"kernel_btvirt_lines={kern_count} cadence_ms={args.cadence_ms:g} "
          f"btvirt_out={btvirt_log.name} kern_log={kern_log}", flush=True)
    return 0 if total_exits == 0 and kern_count == 0 else 1


sys.exit(main())
PY
