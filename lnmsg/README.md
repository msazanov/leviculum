# lnmsg

A terminal LXMF messenger for Reticulum. Plain `lnmsg` opens the
interactive messenger, with conversations, messages and a compose line
(`lnmsg tui` is the same under its old name). The subcommands are for
scripts: `lnmsg send` queues one message for an LXMF address; `lnmsg
fetch` drains the propagation node's mailbox; `lnmsg address` prints this
machine's own address.

lnmsg attaches to a Reticulum shared instance that is already running,
`lnsd` from the `leviculum` package or Python's `rnsd`, the way `lnomad`
does. It runs its own LXMF router but never a Reticulum stack of its own, so
a daemon has to be running.

The `lnmsg` package ships the binary built with the `tui` feature and no
service. Every option is in `lnmsg(1)`; the design is in
`docs/src/concepts/lnmsg.md` and the pages beside it.
