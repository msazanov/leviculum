#!/usr/bin/env python3
"""A stock Python LXMF client uploads to a propagation node over a loopback
shared instance; the proof is timed against the client's own receipt
timeout (leviculum order 709).

The shared instance is the release lnsd. The propagation node is either the
stock lxmd from the pinned reference (`--pn lxmd`, the baseline) or the
release lnpnd (`--pn lnpnd`), both at stamp cost 13. The client is
`scripts/pn_upload_probe.py`, the reference LXMRouter unmodified.

    just build-integ-bins
    scripts/pn-upload-ab.py --pn lxmd
    scripts/pn-upload-ab.py --pn lnpnd

Exit status: the probe's (0 iff every upload reached SENT without a link
teardown); with `--within-timeout`, additionally every proof must arrive
inside the client's receipt timeout. lxmd does not meet that over loopback
(16 ms against 13 ms, 2026-10-13) and survives on the client's one-second
receipt check; lnpnd does (3 ms against 6 ms).
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYPATH = f"{REPO}/reference/Reticulum:{REPO}/reference/LXMF"


def release_bin(name):
    target = os.environ.get("CARGO_TARGET_DIR", os.path.join(REPO, "target"))
    triple = os.environ.get("CARGO_BUILD_TARGET", "x86_64-unknown-linux-musl")
    for path in (os.path.join(target, triple, "release", name),
                 os.path.join(target, "release", name)):
        if os.path.exists(path):
            return path
    sys.exit(f"{name}: no release build under {target}; run `just build-integ-bins`")


RNS_CLIENT = """[reticulum]
  enable_transport = no
  share_instance = Yes
  instance_name = {instance}

[logging]
  loglevel = 2

[interfaces]
"""

PN_CONFIG = """[propagation]
  enable_node = yes
  autopeer = no
  propagation_stamp_cost_target = 13
  propagation_stamp_cost_flexibility = 3
  peering_cost = 4

[lxmf]
  display_name = pn-upload-ab
  announce_at_start = no

[logging]
  loglevel = 7
"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pn", choices=("lxmd", "lnpnd"), required=True)
    parser.add_argument("--count", type=int, default=10)
    parser.add_argument("--within-timeout", action="store_true")
    parser.add_argument("--keep", action="store_true", help="keep the run directory")
    args = parser.parse_args()

    run = tempfile.mkdtemp(prefix=f"pn_upload_ab_{args.pn}_")
    instance = os.path.basename(run)
    for sub in ("lnsd", "client/storage", "pn/rns/storage", "pn/node"):
        os.makedirs(os.path.join(run, sub))
    with open(f"{run}/lnsd/config", "w") as f:
        f.write(f"[reticulum]\n  enable_transport = yes\n  share_instance = Yes\n"
                f"  instance_name = {instance}\n\n[logging]\n  loglevel = 4\n\n[interfaces]\n")
    for path in (f"{run}/client/config", f"{run}/pn/rns/config"):
        with open(path, "w") as f:
            f.write(RNS_CLIENT.format(instance=instance))
    with open(f"{run}/pn/node/config", "w") as f:
        f.write(PN_CONFIG)

    env = dict(os.environ, PYTHONPATH=PYPATH)
    procs = []

    def spawn(cmd, log, extra=None):
        out = open(log, "w")
        proc = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT,
                                env=dict(env, **(extra or {})))
        procs.append(proc)

    try:
        spawn([release_bin("lnsd"), "--config", f"{run}/lnsd", "-v"], f"{run}/lnsd.log")
        identity = f"{run}/lnsd/storage/transport_identity"
        deadline = time.time() + 10
        while not os.path.exists(identity):
            if time.time() > deadline:
                sys.exit("lnsd wrote no transport identity; see " + f"{run}/lnsd.log")
            time.sleep(0.1)
        time.sleep(1.0)
        # The instance's transport identity, so the Python clients derive
        # its RPC key (lxmd_peer.rs does the same).
        shutil.copy(identity, f"{run}/client/storage/transport_identity")
        shutil.copy(identity, f"{run}/pn/rns/storage/transport_identity")

        if args.pn == "lxmd":
            spawn([sys.executable, "-u", "-m", "LXMF.Utilities.lxmd", "--config", f"{run}/pn/node",
                   "--rnsconfig", f"{run}/pn/rns"], f"{run}/pn.log")
            pattern = re.compile(r"LXMF Propagation Node started on <([0-9a-f]{32})>")
        else:
            spawn([release_bin("lnpnd"), "--config", f"{run}/pn/node", "--instance", instance, "-v"],
                  f"{run}/pn.log", {"LEVICULUM_EVENT_LOG": f"{run}/pn-events.log"})
            pattern = re.compile(r"propagation node ready, destination ([0-9a-f]{32})")
        pn = None
        deadline = time.time() + 30
        while pn is None and time.time() < deadline:
            with open(f"{run}/pn.log") as f:
                found = pattern.search(f.read())
            pn = found.group(1) if found else None
            time.sleep(0.1)
        if pn is None:
            sys.exit(f"{args.pn} reported no propagation destination; see {run}/pn.log")
        print(f"PN {args.pn} {pn} run={run}", flush=True)

        probe = subprocess.run(
            [sys.executable, "-u", os.path.join(REPO, "scripts", "pn_upload_probe.py"),
             "--rnsconfig", f"{run}/client", "--pn", pn, "--count", str(args.count)],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            timeout=60 * (args.count + 1))
        print(probe.stdout, end="", flush=True)
        status = probe.returncode
        if args.within_timeout:
            late = [line for line in probe.stdout.splitlines()
                    if line.startswith("UPLOAD") and not proven_in_time(line)]
            for line in late:
                print("LATE " + line, flush=True)
            if late:
                status = status or 1
        return status
    finally:
        for proc in reversed(procs):
            proc.terminate()
        for proc in procs:
            try:
                proc.wait(5)
            except subprocess.TimeoutExpired:
                proc.kill()
        if not args.keep:
            shutil.rmtree(run, ignore_errors=True)


def proven_in_time(line):
    fields = dict(part.split("=", 1) for part in line.split()[1:] if "=" in part)
    try:
        return float(fields["proof_ms"]) < float(fields["timeout_ms"])
    except (KeyError, ValueError):
        return False


if __name__ == "__main__":
    sys.exit(main())
