# Self-hosted infrastructure

A plan to give Leviculum and Periculum a home on a server under our own
control, publicly `leviculum.network`, reachable over both the clearweb
and the Reticulum network. The source of truth moves there, step by step,
while Codeberg runs in parallel; once the move is done, other forges are
mirrors only.

This was first written on 2026-08-17 as a design record. Lew revised it
on 2026-10-28: the source of truth moves to our own server, nothing is
built on that server, and `lblogd` becomes the frontend on both
networks. Part of it is in the tree and running. The next section says
exactly which part, so that the rest can be read as what it is: a plan.

## What of this is already built

- **The server.** `leviculum.network` is a small VPS: 2 vCPU, about
  2 GiB RAM, no swap. It runs `lnsd` and `lblogd`. The `lblogd`
  package ships the site pages (`site/pages/`: landing, download,
  source, boards), served when `pages_dir` points at them.
- **The download path, end to end.** The nightly host runs the tier-2
  nightly every night. After a green or attributed night it runs
  `just nightly-publish` (`scripts/nightly-publish.sh`, 517) in the tree
  that night tested: it builds the packages, the lnflash bundle and the
  book, and `scripts/publish-site.sh` hands them over ssh to
  `packaging/site/lev-receive-nightly`, which runs on the server as the
  forced command of a dedicated receiver user and key. The receiver
  verifies the whole upload before a byte of it is reachable, publishes
  under `releases/nightly/<build-id>/`, moves `nightly/latest` by
  `rename(2)` once the new build is complete, and keeps the last `KEEP`
  (default 14) builds. The host key is pinned, with no accept on first
  use. This has published to
  `https://leviculum.network/releases/nightly/latest/` since 517.
- **The book**: the same publish carries it as a tarball, and
  `packaging/site/lev-unpack-docs` makes it what `/docs/` serves.

Unbuilt: a page in `lblogd` that lists the releases with their notes,
the `issues/` store and its import, the bare git repo on the server,
`rngit`, and `AGENTS.md`.

## Motivation

Today the project is single-homed: code, release artifacts and the issue
tracker all live with one hosting provider. Any single provider is a
single point of failure. An outage, an account dispute, or a change in a
service's terms can remove all three at once. The response is to stop
being single-homed: an independent home we fully control holds the
source of truth, and hosted forges carry copies of it, so the loss of
any one of them is a non-event. As a bonus, a self-hosted node is
reachable over Reticulum, which clearweb forges are not, a natural fit
for a Reticulum stack.

## Principles

1. **Simplest thing that works.** No forge platform, no CI framework on
   the server. A bare git repo, plain files, `lblogd`, `rngit`.
2. **Both networks, one source.** Everything a user needs (code, issues,
   releases) is reachable over clearweb *and* Reticulum. Where possible
   the data lives *in the git repo*, which is clonable over both, and one
   daemon renders it for both, rather than one service per network.
3. **Nothing is built on the server.** It is too small for the workspace
   and the firmware, and the gate that makes a build green already runs
   on the nightly host. The server receives finished, tested builds.
4. **No single point of failure at the code.** The authoritative copy is
   an ordinary bare git repo. `rngit`, young and "not tested extensively
   in the wild" per the RNS manual, sits *beside* it as a Reticulum
   remote, never underneath it.
5. **AI-friendly by construction.** An agent with no prior knowledge
   reads one `AGENTS.md` and knows how to clone, file an issue, and find
   a release, because the patterns are plain files and standard git, not
   a bespoke API.
6. **Reticulum compatibility is Priority 1.** The Reticulum side speaks
   NomadNet pages (`lblogd`) and Mark's own `rngit`, so we interoperate
   with the ecosystem by default.

## Architecture

```
  nightly host                         leviculum.network (small VPS)
  ┌───────────────────────┐            ┌──────────────────────────────────┐
  │ tier-2 nightly        │            │ lev-receive-nightly (forced cmd) │
  │  gate green/attrib.   │   ssh      │   releases/nightly/<build-id>/   │
  │  just nightly-publish ├──────────► │   releases/nightly/latest        │
  │   build debs, bundle, │            │                                  │
  │   book; publish-site  │            │ lblogd   web (HTTP/HTTPS)        │
  └───────────────────────┘            │          NomadNet node           │
                                       │   site pages, releases, issues   │
  maintainers ── ssh push ───────────► │ /srv/git/leviculum.git (bare)    │
                                       │ rngit    rns:// remote           │
                                       │ lnsd     Reticulum transport     │
                                       └──────────────────────────────────┘
  Codeberg, other forges: mirrors of the bare repo (Codeberg in parallel
  until the move is done; its issues mirrored one way into issues/).
```

## Component 1: git upstream

The authoritative copy becomes `/srv/git/leviculum.git`, a bare repo on
the server.

- **Maintainer push:** over SSH, key auth, no web push. The only write
  path to the code.
- **Clearweb read/clone:** smart HTTP via `git-http-backend` (a CGI
  shipped with git), or dumb HTTP (`git update-server-info` in a
  post-receive hook plus static serving) for zero CGI. Anonymous
  clone/pull only.
- **Reticulum read/clone:** `rngit` with `[repositories] public = /srv/git`
  exposes the same repo at `rns://<hash>/public/leviculum`.
- **Introduced as the second push remote first**
  (`git remote set-url --add --push origin`), so Codeberg and the server
  hold identical history; content addressing makes the two byte-identical
  when in sync. Once it has carried every push for a while, it becomes
  the first remote and Codeberg the mirror.
- **Other forges are mirrors only.** A mirror on another hosted forge
  (CodeFloe was named) may exist for public findability. None is ever
  the source of truth.

Periculum is a second repo in the same group, treated identically.

## Component 2: issues as plain text in the repo

Issues are Markdown files committed into the repo. They are part of every
clone, on both networks, with no running issue service and no API.

```
issues/
  open/    0042-lxmf-msgpack-unbounded-recursion.md
  closed/  0038-lrproof-hop-asymmetry.md
```

Each file is front matter plus a Markdown body:

```markdown
---
id: 42
title: LXMF msgpack skip recurses without a depth budget
labels: [bug, compat, priority:low]
state: open
created: 2026-08-17
author: <name or Reticulum identity hash>
---
Body in Markdown, cite code as path:line. Comments are appended below a
`---` rule, each with an author + date line.
```

- **State** is the directory (`open/` vs `closed/`); `ls issues/open` is
  the backlog, a state change is one `git mv`, and it diffs cleanly.
- **Search** is `grep -r`. No index, no database.
- **Numbering:** `scripts/new-issue.sh "title"` picks the next free
  number by scanning `issues/`. The imported Codeberg issues keep their
  numbers, so every `#N` already written in commits and docs still
  names the same issue. Collisions are rare (nearly all issues originate
  from our own instances) and resolve at merge; switch to short hash IDs
  only if distributed creation ever makes that a real problem.
- **Attribution**, if wanted, comes from signed git commits, not a
  per-issue crypto layer.

`rngit work` was considered and rejected for the issue store: it keeps
work items as msgpack in a side directory, writable only with `rngit`
installed and not legible at rest. Plain files win on reach over both
networks, AI transparency, and zero service, at the cost of built-in
signing, which git commit signing replaces.

## Component 3: releases are built on the nightly host

The 2026-08-17 design had a `nightly.sh` on the server, run by a systemd
timer: check out master, build, test, publish on green. That is replaced
by what already runs. The nightly host builds and tests every night, and
after a green or attributed night `just nightly-publish` builds the
packages from the commit that night tested and deploys them to the
server (see *What of this is already built*). Two reasons it stays that
way:

- **The server is too small.** 2 vCPU and about 2 GiB RAM without swap
  do not build the workspace, let alone the firmware, and a build there
  would compete with the `lnsd` and `lblogd` it is meant to serve.
- **The gate already runs elsewhere.** What makes a build green is the
  tier-2 nightly. A second build and test on the server would either
  repeat that gate on weaker hardware or publish without it.

The three properties the old design was written for hold on this path:
`PUB-0016` (nothing ships from an untested or red tree:
`scripts/check-nightly-green.sh` refuses a commit no green night
covers), `PUB-0005` (no destroy before upload: the receiver points
`latest` only after the new build is complete) and `PUB-0011` (rollback:
the retained builds and the `latest` link are exactly that).

The Woodpecker pipeline has no job in the release path to the server.
Its `publish-site` step in `.woodpecker/nightly.yml` is a manual
fallback from before 517 and goes with this move.

## Component 4: downloads over both networks

- **Clearweb:** stable URLs under
  `https://leviculum.network/releases/nightly/latest/`, served from the
  receiver's tree, `/var/www/leviculum/releases` (the default
  `RELEASES_ROOT` of `packaging/site/lev-receive-nightly`).
- **Release pages on both networks:** `lblogd` lists the releases, shows
  their notes and links the downloads, on the web and as NomadNet pages
  (Component 5). This takes the place of `rngit`'s `serve_nomadnet`
  release list in the 2026-08-17 design.
- **Reticulum file pull:** `lblogd`'s file area already serves files
  under `/file/<name>` in the form NomadNet's `serve_file` uses.
  `rngit release <repo> fetch` (which verifies an Ed25519 manifest
  signature before writing any bytes) and `lncp`/`rncp --fetch` stay
  available for raw pulls.

## Component 5: `lblogd` as the frontend

`lblogd` already serves Markdown over HTTP and HTTPS and as a NomadNet
node from one snapshot, and already carries the site pages
(`docs/src/man/lblogd.1.md`). It becomes the project's frontend on both
networks, from one source: first the releases (list, notes, download),
later the issues, rendered from `issues/`. The 2026-08-17 design had two
static views instead, `stagit` for code browsing and a separate script
rendering `issues/` to HTML; both are superseded by this frontend. Code
browsing over the web is a clone or a forge mirror until a need for more
is shown.

What `lblogd/src` holds today, split by whether a project site would
need it:

| module       | blog-specific | a project site shares |
|--------------|---------------|-----------------------|
| `render.rs`  | the index, post, about and Atom feed templates; `BlogMeta` | `markdown_to_html`, `markdown_to_micron`, the page and link templates, the nav line, the AGPL source offer, the default stylesheet |
| `post.rs`    | dates, byline, the post index order | the front matter parser and the slug rules, which static pages already use |
| `content.rs` | `posts` and `about` in the `Snapshot` | the one snapshot both sides read, the atomic reload that keeps the old content on a failed load |
| `site.rs`    | the `blog`, `posts` and `feed.xml` entries of `RESERVED` | `pages_dir`, `[links]`, the one table of taken names checked for both sides |
| `node.rs`    | the names (`BlogNode`, the display name) | the whole page node: shared-instance client, one handler per path, Resource fallback, the file area on the mesh |
| `web.rs`     | the `/blog`, `/posts/<slug>`, `/about`, `/feed.xml` routes | the listener, ACME via rustls-acme, the plain mode behind a proxy, `/files/`, the counting layer |
| `files.rs`, `counter.rs`, `watcher.rs` | nothing | all of it |
| `config.rs`  | the `[blog]` section | the `[node]`, `[web]`, `[counter]`, `[source]` sections |

The blog is a thin layer, a few templates and routes, over a general
"Markdown on both networks" server. That is the seam.

**Open for Lew: extend `lblogd`, or split a project site from it.**

- **(a) Extend `lblogd`.** Releases and issues become further content
  sources beside posts and pages. One daemon, one package, no new crate.
  But every release and issue concept becomes config surface and reserved
  names of a product others run as a pure blog, with its own version and
  compatibility surface (`lblogd/Cargo.toml`).
- **(b) Split.** The shared column above moves into a library crate (a
  two-network Markdown site: renderers, snapshot and reload, page node,
  web listener, file area, counter, watcher, pages and links). `lblogd`
  stays the blog on top of it; a second daemon, the project site, adds
  releases and issues on top of the same crate. No code is copied: the
  rule is no duplicated code, so a split means the library, not a fork.
  The cost is one crate boundary to design and a second package and
  service on the server.
- **(c) Neither yet.** The first increment, the release list, needs no
  new code in `lblogd`: the receiver already knows each build it
  publishes, so it can write a Markdown page into `pages_dir` and reload
  `lblogd`, which serves it on both networks as it serves the download
  page today.

**Recommendation:** (c) for the releases now, and (b) when the issues
come. A release list is one page; the issues are hundreds of entries
with an index, labels and states, a collection like the posts, and that
is the point where (a) would turn `lblogd` into a forge frontend and
stop being a blog server others can simply run. Cutting the library
along the table above at that point costs one refactor with the blog's
existing tests as its guard.

## Component 6: Codeberg in parallel

Code sync is trivial: the same commits go to both remotes, and content
addressing guarantees they match.

Issues are the hard part, because Codeberg keeps them in its database
while ours are files in the repo. Decided 2026-10-28: while both live,
the sync is **one way, Codeberg to repo**. A one-time import brings the
current Codeberg issues into `issues/`, and a job on a timer mirrors
later creates, edits, comments and closes into the files, so Reticulum
users can read them and `lblogd` can render them. When the move is done
the repo becomes the source of truth and Codeberg the mirror. The job is
an export/import script, not a live service; the Gitea API access already
used for issue curation is sufficient.

## AI-friendliness: the `AGENTS.md` contract

A single file at the repo root tells any agent everything in one screen:
how to clone over each network, that issues are Markdown under
`issues/open` and `issues/closed`, how to file one (`scripts/new-issue.sh`,
edit, commit, push), how to close one (`git mv` to `closed/` with a
note), and that releases are cut only by `just nightly-publish` on the
nightly host after a green or attributed night. Because the substrate is
plain files and standard git, an agent needs no project-specific API
knowledge.

**Where this stands.** None of that is in the tree: no `AGENTS.md`, no
`issues/` directory, no `scripts/new-issue.sh`. It comes last in the
rollout, because it describes the other three. Said here because the
paragraph above reads as a description of the repository, and the
citation guard cannot catch that: it names files in prose rather than as
`path:line` citations, so a reader is the only check there is.

## Rollout

Step by step, each step in parallel with Codeberg, no forge installed.

1. **Releases.** Receiver user and key on the server, the nightly host
   deploying after a green or attributed run: done. Left: `lblogd` shows
   the releases (list, notes, download) on both networks, and the
   Woodpecker `publish-site` fallback goes.
2. **Issues.** The one-time import from Codeberg into `issues/`, the one
   way mirror Codeberg to repo while both live, `lblogd` (or the project
   site, see Component 5) renders them.
3. **Git.** The bare repo on the server as the second push remote, then
   as the first; `rngit` beside it; Codeberg and any other forge as
   mirrors.
4. **`AGENTS.md`**, describing the three steps as they then stand.

## What this resolves

- Single-provider dependency for code, releases and issues: the reason
  for a home under our own control.
- Untested nightly builds, destroy-before-upload publishing, and no
  rollback: designed out by the publish path of Component 3.
- Build docs that point at a path the musl target never produces:
  folded into the README and `AGENTS.md`.

## Open questions

1. **`lblogd` or a project site** (Component 5): extend `lblogd`, or
   split a shared library crate and a second daemon. Recommendation:
   generated pages for the releases now, the split when the issues come.
2. **Reticulum transport to the server:** TCP interface over the
   clearweb, or a real RF path; affects reachability and announce
   cadence of `lblogd`'s node and `rngit`.
3. **Issue import field mapping:** how Codeberg's labels, milestones,
   assignees and comment authors map onto the front matter, and what the
   mirror does with an edit to an issue already imported.
4. **`rngit` acceptance bar:** what track record over Reticulum is
   required before it is relied on even as a secondary path.
5. **Signing:** SSH vs GPG commit signing for attribution; whether
   release signing uses a dedicated identity.

Closed on 2026-10-28: the server baseline (a small VPS already running
`lnsd` and `lblogd`, nothing built on it), and the issue sync direction
(one way, Codeberg to repo, while both live).

## See also

- [Zero-config lnsd](zero-config.md): `leviculum.network` is the first
  entry of the server list every fresh install dials, and what that
  entry owes (monitoring, a privacy statement, consent of further
  operators) before it ships.
- [lblogd(1)](../man/lblogd.1.md): the daemon that is the frontend on
  both networks.
