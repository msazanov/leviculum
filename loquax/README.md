# loquax

A terminal LXMF messenger for Reticulum. Plain `loquax` opens the
interactive messenger, with conversations, messages and a compose line
(`loquax tui` is the same under its old name). The subcommands are for
scripts: `loquax send` queues one message for an LXMF address; `loquax
fetch` drains the propagation node's mailbox; `loquax address` prints this
machine's own address.

loquax attaches to a Reticulum shared instance that is already running,
`lnsd` from the `leviculum` package or Python's `rnsd`, the way `lnomad`
does. It runs its own LXMF router but never a Reticulum stack of its own, so
a daemon has to be running.

The `loquax` package ships the binary built with the `tui` feature and no
service. Every option is in `loquax(1)`; the design is in
`docs/src/concepts/loquax.md` and the pages beside it.
