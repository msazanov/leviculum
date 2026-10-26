# Python-RNS storage fixtures

Files the reference rnsd writes under `<configdir>/storage`, read by the
`test_python_*_compat` lib tests in `leviculum-std`
(`known_destinations.rs`, `packet_hashlist.rs`, `driver/builder.rs`).
They replace a read of the host's `~/.reticulum/storage`, which made the
tests vacuous without a Python rnsd and red when its history was empty.

Reference: Python-RNS 1.3.5 (`reference/Reticulum`). Written by

    python3 scripts/make-python-storage-fixtures.py \
        leviculum-std/tests/fixtures/python-storage

which runs two throwaway Reticulum instances in TempDir configs joined over
loopback TCP. The receiver (transport enabled) learns one announce with
`app_data = b"leviculum fixture"` and persists through the reference's own
exit handler.

| File                 | Bytes | Content                                      |
|----------------------|-------|----------------------------------------------|
| `packet_hashlist`    | 69    | umsgpack list of two 32-byte packet hashes   |
| `known_destinations` | 157   | umsgpack map, one destination, five fields   |
| `transport_identity` | 64    | raw X25519 and Ed25519 private keys          |

The identity is throwaway and belongs to no node.
