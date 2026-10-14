#!/usr/bin/env python3
"""Upload messages from a stock Python LXMF client to a propagation node and
time every upload's proof against the client's own receipt timeout
(leviculum order 709).

The client is the reference `LXMRouter`, unmodified. What this script adds is
observation only: it records each link `PacketReceipt` the client creates
(`RNS/Packet.py:418`), every `check_timeout` pass over it (`:560`, called
from the transport job loop once per `receipts_check_interval`,
`RNS/Transport.py:556-573`), and every link teardown. The client's own
behaviour, timeouts and retries are untouched.

Usage: pn_upload_probe.py --rnsconfig DIR --pn HEX [--count N]

Prints one line per upload,

    UPLOAD n=<i> state=<final> proof_ms=<ms|none> timeout_ms=<ms>
           check_ms=<first check after the timeout, ms> teardowns=<k>

and a final `PROBE_SUMMARY` line. Exit status 0 iff every upload reached SENT
without a link teardown.
"""

import argparse
import os
import sys
import tempfile
import threading
import time

import RNS
import LXMF

RECEIPTS = []
TEARDOWNS = []
LOCK = threading.Lock()

_receipt_init = RNS.PacketReceipt.__init__
_check_timeout = RNS.PacketReceipt.check_timeout
_link_teardown = RNS.Link.teardown


def receipt_init(self, packet):
    _receipt_init(self, packet)
    if packet.destination.type == RNS.Destination.LINK:
        self.probe_checks = []
        with LOCK:
            RECEIPTS.append(self)


def check_timeout(self):
    if hasattr(self, "probe_checks"):
        self.probe_checks.append(time.time())
    return _check_timeout(self)


def link_teardown(self):
    with LOCK:
        TEARDOWNS.append(time.time())
    return _link_teardown(self)


RNS.PacketReceipt.__init__ = receipt_init
RNS.PacketReceipt.check_timeout = check_timeout
RNS.Link.teardown = link_teardown

STATE_NAMES = {
    LXMF.LXMessage.GENERATING: "GENERATING",
    LXMF.LXMessage.OUTBOUND: "OUTBOUND",
    LXMF.LXMessage.SENDING: "SENDING",
    LXMF.LXMessage.SENT: "SENT",
    LXMF.LXMessage.DELIVERED: "DELIVERED",
    LXMF.LXMessage.REJECTED: "REJECTED",
    LXMF.LXMessage.CANCELLED: "CANCELLED",
    LXMF.LXMessage.FAILED: "FAILED",
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rnsconfig", required=True)
    parser.add_argument("--pn", required=True)
    parser.add_argument("--count", type=int, default=10)
    parser.add_argument("--per-upload-timeout", type=float, default=60.0)
    args = parser.parse_args()

    RNS.Reticulum(configdir=args.rnsconfig, loglevel=RNS.LOG_WARNING)
    storage = tempfile.mkdtemp(prefix="pn_upload_probe_")
    router = LXMF.LXMRouter(identity=RNS.Identity(), storagepath=storage)
    source = router.register_delivery_identity(RNS.Identity(), display_name="probe")
    recipient = RNS.Destination(
        RNS.Identity(), RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery"
    )

    pn_hash = bytes.fromhex(args.pn)
    deadline = time.time() + 60
    while not (RNS.Transport.has_path(pn_hash) and RNS.Identity.recall(pn_hash)):
        if time.time() > deadline:
            print("NO_PATH pn=" + args.pn, flush=True)
            return 2
        RNS.Transport.request_path(pn_hash)
        time.sleep(0.5)
    router.set_outbound_propagation_node(pn_hash)
    print(f"PN_KNOWN pn={args.pn} cost={router.get_outbound_propagation_cost()}", flush=True)

    clean = 0
    for n in range(args.count):
        with LOCK:
            receipts_before = len(RECEIPTS)
            teardowns_before = len(TEARDOWNS)
        message = LXMF.LXMessage(
            recipient, source, f"probe upload {n}", "probe",
            desired_method=LXMF.LXMessage.PROPAGATED,
        )
        router.handle_outbound(message)
        states = []
        end = time.time() + args.per_upload_timeout
        while time.time() < end:
            if not states or states[-1] != message.state:
                states.append(message.state)
            if message.state in (LXMF.LXMessage.SENT, LXMF.LXMessage.DELIVERED,
                                 LXMF.LXMessage.FAILED, LXMF.LXMessage.REJECTED):
                break
            # A torn-down upload parks the message as OUTBOUND (LXMessage.py:616-621);
            # the router retries it later. Stop at the first teardown.
            with LOCK:
                if len(TEARDOWNS) > teardowns_before:
                    break
            time.sleep(0.002)
        time.sleep(0.3)
        with LOCK:
            receipts = RECEIPTS[receipts_before:]
            teardowns = len(TEARDOWNS) - teardowns_before
        state = STATE_NAMES.get(message.state, str(message.state))
        if receipts:
            receipt = receipts[0]
            timeout_ms = receipt.timeout * 1000
            proof_ms = (
                f"{(receipt.concluded_at - receipt.sent_at) * 1000:.1f}"
                if receipt.status == RNS.PacketReceipt.DELIVERED and receipt.concluded_at
                else "none"
            )
            expired = [t for t in receipt.probe_checks if t > receipt.sent_at + receipt.timeout]
            check_ms = f"{(expired[0] - receipt.sent_at) * 1000:.1f}" if expired else "none"
            timeout_txt = f"{timeout_ms:.1f}"
        else:
            proof_ms, check_ms, timeout_txt = "none", "none", "none"
        trail = ">".join(STATE_NAMES.get(s, str(s)) for s in states)
        print(
            f"UPLOAD n={n} state={state} proof_ms={proof_ms} timeout_ms={timeout_txt} "
            f"check_ms={check_ms} receipts={len(receipts)} teardowns={teardowns} trail={trail}",
            flush=True,
        )
        if state in ("SENT", "DELIVERED") and teardowns == 0:
            clean += 1

    print(f"PROBE_SUMMARY uploads={args.count} clean={clean}", flush=True)
    sys.stdout.flush()
    os._exit(0 if clean == args.count else 1)


if __name__ == "__main__":
    sys.exit(main())
