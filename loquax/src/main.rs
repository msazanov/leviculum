//! `loquax` binary: the terminal LXMF messenger described in
//! `docs/src/concepts/loquax.md`. With no subcommand it is the conversation
//! view (`tui`, with the `tui` feature); the subcommands are the paths for
//! scripts:
//!
//! ```sh
//! echo "disk 91%" | loquax send <address> --title "backup"
//! loquax fetch
//! ```
//!
//! It attaches to a running `lnsd` or `rnsd` shared instance the way `lnomad`
//! does, runs one command through its own LXMF router and exits. A send that
//! worked prints nothing at all; errors go to stderr. It never starts a
//! Reticulum stack of its own, and it never claims a message was delivered.
//!
//! Exit codes extend `lnomad`'s convention (`lnomad/src/main.rs:174`, `:188`,
//! `:222`): 0 success, 1 operational failure, 2 argument error — plus 3,
//! "a propagation node holds the message", which `--via auto`'s fallback and
//! `--via propagated` return so a script can tell the two successes apart
//! (`loquax/src/send.rs`, `EXIT_PROPAGATED`).

#[cfg(feature = "gui")]
mod gui;
#[cfg(any(feature = "tui", feature = "gui"))]
mod shell;
#[cfg(feature = "tui")]
mod tui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use leviculum_std::config::Config;

use loquax::clock::Tokio;
use loquax::effects::Terminal;
use loquax::engine::{attach, AttachConfig};
use loquax::fetch::{run_fetch, FetchError, FetchOptions};
use loquax::mailbox_state::{self, MailboxRun, Route};
use loquax::outbox::{MailboxInput, SendRequest, Via};
use loquax::seen::SeenStore;
use loquax::send::{
    run_send, run_send_auto, run_send_propagated, AutoError, AutoSent, SendOptions, EXIT_FAILURE,
    EXIT_PROPAGATED, EXIT_USAGE,
};
use loquax::{address, body, display_name, events, identity};

/// Everything after attaching gets this many seconds by default: becoming
/// ready, learning a route to the destination, queueing, and getting the
/// message onto the network. A cron job needs to know when the command is
/// guaranteed to be gone more than it needs a slow path to succeed. In
/// `--via auto` the direct leg and the fallback leg each get one budget.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// `loquax fetch`'s default budget: a drain crosses the link twice for the
/// list round alone and the payload can be a mailbox-full, so it gets twice
/// the send budget before it is called stuck.
const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 60;

#[derive(Parser, Debug)]
#[command(
    name = "loquax",
    version = env!("LEVICULUM_VERSION"),
    about = "the Leviculum messenger; with no arguments it opens the conversation view",
    long_about = "the Leviculum messenger; with no arguments it opens the conversation view\n\n\
                  `send`, `fetch` and `address` are the commands for scripts. \
                  loquax attaches to a daemon that is already running (lnsd, or \
                  Python's rnsd) the same way lnomad does. It does not start a \
                  Reticulum stack of its own, so a daemon has to be running.\n\n\
                  A send that worked prints nothing and exits 0, which means \
                  the message was queued cleanly -- not that it was delivered. \
                  Exit 3 means a propagation node holds the message for later \
                  collection. Failures explain themselves on stderr. Set \
                  LEVICULUM_EVENT_LOG=<path> for the full run as structured \
                  events, including the message id."
)]
// No subcommand is the messenger (Lew, 2026-10-26), so the subcommand is an
// `Option` and the messenger's options sit at the top level; `tui` stays as
// a subcommand so every script and cell that names it keeps working.
// `args_conflicts_with_subcommands` keeps the two forms apart: `loquax --to X
// send` is an argument error, not a send with a stray flag.
#[command(args_conflicts_with_subcommands = true)]
struct Args {
    /// Run as the home node, headless, until SIGINT or SIGTERM: receive for
    /// this identity, archive every message, answer the admitted devices,
    /// collect from the propagation node. Takes the messenger's --instance,
    /// --config, --pn, --sync-interval and --timeout.
    #[cfg(feature = "tui")]
    #[arg(long)]
    daemon: bool,

    #[cfg(feature = "tui")]
    #[command(flatten)]
    tui: TuiArgs,

    #[command(subcommand)]
    command: Option<SubCommand>,
}

#[derive(Subcommand, Debug)]
enum SubCommand {
    /// Queue one message for an LXMF address.
    Send(SendArgs),
    /// Drain the propagation node's mailbox and print what is new.
    Fetch(FetchArgs),
    /// Print this machine's own LXMF address.
    Address,
    /// The terminal messenger, as plain `loquax`: conversations, messages and
    /// a compose line.
    #[cfg(feature = "tui")]
    Tui(TuiArgs),
    /// The messenger in a window, attached as the terminal one is. Its
    /// first view is the map of every peer with a position.
    #[cfg(feature = "gui")]
    Gui(GuiArgs),
    /// The home node and its devices: admit, join, devices, revoke.
    #[cfg(feature = "tui")]
    #[command(subcommand)]
    Home(HomeCommand),
}

/// `loquax home ...`.
#[cfg(feature = "tui")]
#[derive(Subcommand, Debug)]
enum HomeCommand {
    /// On the home node: open a single-use admission for one device and
    /// print its code.
    Admit {
        /// The name the device is listed and revoked under.
        #[arg(long)]
        name: Option<String>,
    },
    /// On the new device: redeem a code from `home admit` and become a
    /// client of that home node.
    Join(JoinArgs),
    /// On the home node: list the admitted devices.
    Devices,
    /// On the home node: remove a device, by its name or its hash (four hex
    /// characters of it are enough when they are unique).
    Revoke { device: String },
}

#[cfg(feature = "tui")]
#[derive(clap::Args, Debug)]
struct JoinArgs {
    /// The code `loquax home admit` printed on the home node.
    code: String,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds for the path, the link and the answer.
    #[arg(long, default_value_t = DEFAULT_FETCH_TIMEOUT_SECS)]
    timeout: u64,
}

#[cfg(feature = "gui")]
#[derive(clap::Args, Debug)]
struct GuiArgs {
    /// The propagation node for the fallback and for fetch, as `send --pn`.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds each send's leg may take, as `send --timeout`; a fetch gets
    /// twice this.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,

    /// The longest the mailbox goes unsynced, as the terminal's
    /// `--sync-interval`: `15m` by default, `off` for none.
    #[arg(long, value_name = "DURATION")]
    sync_interval: Option<String>,
}

#[cfg(feature = "tui")]
#[derive(clap::Args, Debug)]
struct TuiArgs {
    /// A correspondent to list from the start: 32 hex characters, repeatable.
    /// The history restores every conversation already held, so this is for
    /// writing first to someone it does not hold yet.
    #[arg(long, value_name = "ADDRESS")]
    to: Vec<String>,

    /// The propagation node for the fallback and for fetch, as `send --pn`.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds each send's leg may take, as `send --timeout`; a fetch gets
    /// twice this.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,

    /// The longest the mailbox goes unsynced: `15m` (the default, over the
    /// config file's `sync_interval`), `90s`, `1h`, minutes without a unit;
    /// `off` or `0` syncs only at start and on Ctrl-r. After a message in or
    /// out it syncs more often for a while, never less often.
    #[arg(long, value_name = "INTERVAL")]
    sync_interval: Option<String>,

    /// Draw the delivery marks in ASCII (`.` `*` `o` `^` `v` `x`).
    #[arg(long)]
    ascii: bool,

    /// No colours: the monochrome screen, reverse video for the selection
    /// and the toast. `NO_COLOR` set and not empty does the same.
    #[arg(long)]
    no_color: bool,

    /// The colour tier: `auto` climbs the ladder (`COLORTERM` truecolor or
    /// 24bit, `TERM` with 256color, none on a dumb or unset `TERM`, else the
    /// sixteen); `truecolor`, `256` and `16` force one. `--no-color` and
    /// `NO_COLOR` still win.
    #[arg(long, value_enum, default_value_t = ColorArg::Auto)]
    color: ColorArg,

    /// Light or dark colours: `auto` asks the terminal for its background
    /// (dark when it does not answer). Over the config file's `theme` key.
    #[arg(long, value_enum)]
    theme: Option<ThemeArg>,

    /// Leave the mouse to the terminal, so its own selection and copy work;
    /// the palette's `mouse` captures it again.
    #[arg(long)]
    no_mouse: bool,

    /// How the map is drawn: `auto` picks by itself, asking the terminal
    /// and climbing the ladder, kitty or sixel, else half-blocks in colour,
    /// else braille, which is also what it takes behind mosh; the others
    /// force one.
    #[arg(long, value_enum, default_value_t = tui::map::GraphicsFlag::Auto)]
    graphics: tui::map::GraphicsFlag,

    /// Run a key script instead of reading the keyboard, and write every
    /// `frame` it asks for to --frames. For tests and demos.
    #[arg(long, value_name = "FILE")]
    script: Option<PathBuf>,

    /// Where a script's frames go (default: the script's path with
    /// `.frames`).
    #[arg(long, value_name = "FILE", requires = "script")]
    frames: Option<PathBuf>,

    /// A script's screen size, columns x rows.
    #[arg(long, value_name = "WxH", default_value = "80x24", requires = "script")]
    size: String,
}

#[derive(clap::Args, Debug)]
struct SendArgs {
    /// Recipient's LXMF address: 32 hex characters, with or without the
    /// `lxmf@` prefix lnomad copies to the clipboard.
    address: String,

    /// Message body. Omitted, or given as `-`, the body is read from stdin —
    /// which is the form a script uses: `echo "disk 91%" | loquax send <addr>`.
    /// One trailing newline is removed.
    body: Option<String>,

    /// Message title (LXMF carries one; it may be empty).
    #[arg(long)]
    title: Option<String>,

    /// The name recipients see as the sender. Defaults to the account name of
    /// the user running loquax; `LNMSG_DISPLAY_NAME` sets it where there is no
    /// command line to edit, and this flag wins over it.
    #[arg(long, value_name = "NAME")]
    from: Option<String>,

    /// How the message travels. `auto` tries the direct delivery link first
    /// and falls back to a propagation node when none comes up; `direct` and
    /// `propagated` are the explicit choices.
    #[arg(long, value_enum, default_value_t = ViaArg::Auto)]
    via: ViaArg,

    /// The propagation node to use: 32 hex characters, remembered as your
    /// choice. Without it, the `propagation_node` key in loquax's config
    /// decides; without either, the mailbox loquax adopted on an earlier run,
    /// or a newly adopted one: the most recently heard node that does not
    /// run on this device.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default, like `lncp`).
    /// Only its instance name is read; loquax keeps its own state elsewhere.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds to spend, after connecting, on getting the message onto the
    /// network. This is not a wait for delivery: the command returns as soon
    /// as the stack has taken the message. In `--via auto` each leg gets
    /// this budget.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout: u64,
}

#[derive(clap::Args, Debug)]
struct FetchArgs {
    /// The propagation node to drain: 32 hex characters. Resolution order is
    /// the same as `send --pn`'s.
    #[arg(long, value_name = "HASH")]
    pn: Option<String>,

    /// Shared-instance name to connect to (overrides the config file's).
    #[arg(long)]
    instance: Option<String>,

    /// Reticulum config directory (default: the platform default).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Seconds for the whole drain: selecting the node, the link, the
    /// list/fetch/confirm round.
    #[arg(long, default_value_t = DEFAULT_FETCH_TIMEOUT_SECS)]
    timeout: u64,
}

/// The `--via` choice. `auto` is frontend orchestration over the seam's
/// [`Via`] values, so it has no engine-side mirror.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ViaArg {
    /// Direct link first; a propagation node when none comes up.
    Auto,
    /// Straight to the recipient over the mesh, never a mailbox.
    Direct,
    /// Through a propagation node's mailbox, never a direct attempt.
    Propagated,
}

/// The `--color` choice.
#[cfg(feature = "tui")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ColorArg {
    /// Climb the ladder.
    Auto,
    /// 24-bit true colour.
    Truecolor,
    /// The xterm-256 palette.
    #[value(name = "256")]
    Ansi256,
    /// The sixteen ANSI colours.
    #[value(name = "16")]
    Ansi16,
}

#[cfg(feature = "tui")]
impl From<ColorArg> for leviculum_ui::ColorFlag {
    fn from(arg: ColorArg) -> Self {
        match arg {
            ColorArg::Auto => Self::Auto,
            ColorArg::Truecolor => Self::Truecolor,
            ColorArg::Ansi256 => Self::Ansi256,
            ColorArg::Ansi16 => Self::Ansi16,
        }
    }
}

/// The `--theme` choice.
#[cfg(feature = "tui")]
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ThemeArg {
    /// Ask the terminal.
    Auto,
    /// Dark colours.
    Dark,
    /// Light colours.
    Light,
}

#[cfg(feature = "tui")]
impl From<ThemeArg> for leviculum_ui::ThemeFlag {
    fn from(arg: ThemeArg) -> Self {
        match arg {
            ThemeArg::Auto => Self::Auto,
            ThemeArg::Dark => Self::Dark,
            ThemeArg::Light => Self::Light,
        }
    }
}

fn usage(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("loquax: {message}");
    events::done(None, "usage", EXIT_USAGE);
    ExitCode::from(EXIT_USAGE)
}

fn failure(message: impl std::fmt::Display, outcome: &str) -> ExitCode {
    eprintln!("loquax: {message}");
    events::done(None, outcome, EXIT_FAILURE);
    ExitCode::from(EXIT_FAILURE)
}

/// The instance name decides the abstract socket (`\0rns/{name}`) the daemon
/// listens on, so it comes from the daemon's own config file. Same derivation
/// `lncp`, `lnstatus` and `leviculum-lxmf-node` use.
fn instance_name(config_dir: &std::path::Path) -> String {
    let config_file = config_dir.join("config");
    if config_file.exists() {
        if let Ok(config) = Config::load(&config_file) {
            return config.reticulum.instance_name;
        }
    }
    "default".to_string()
}

/// Resolve the propagation-node choice: the `--pn` flag, then loquax's own
/// config. `None` with `"mailbox"` means "let the mailbox rule decide"
/// (`loquax::mailbox`). The source word is what `LNMSG_PN` logs.
fn resolve_pn(
    flag: Option<&str>,
    home: &std::path::Path,
) -> Result<(Option<[u8; 16]>, &'static str), String> {
    if let Some(text) = flag {
        return match address::parse(text) {
            Ok(hash) => Ok((Some(hash), "flag")),
            Err(error) => Err(format!("--pn {}: {error}", text.trim())),
        };
    }
    match loquax::config::load(home) {
        Ok(config) => match config.propagation_node {
            Some(hash) => Ok((Some(hash), "config")),
            None => Ok((None, "mailbox")),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// How this run selects its mailbox. A run that names a node with `--pn` or
/// the config needs no input; one that does not reads the daemon's path
/// table once, here, because the engine runs under the core lock and may not
/// make a round trip. `excluded` are the nodes that stopped answering, which
/// only the TUI, running sync after sync, keeps; a CLI run has none.
async fn mailbox_run(
    home: &Path,
    config_dir: &Path,
    instance: &str,
    preferred: Option<[u8; 16]>,
    excluded: Vec<[u8; 16]>,
) -> MailboxRun {
    let input = match preferred {
        Some(_) => None,
        None => Some(MailboxInput {
            routes: read_routes(config_dir, instance).await,
            previous: mailbox_state::load(home),
            excluded,
        }),
    };
    MailboxRun { input }
}

/// The client node's storage directory under `home`, created. One that
/// cannot be made fails the run with the outcome a missing daemon has, as it
/// did while `attach` made it.
fn storage_dir(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join("storage");
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}

/// The daemon's path table over the shared-instance RPC, keyed the way
/// `lnstatus` and `lnpath` key themselves (`leviculum_std::daemon_rpc`).
async fn read_routes(config_dir: &Path, instance: &str) -> Result<Vec<Route>, String> {
    let config_file = config_dir.join("config");
    let config = config_file
        .exists()
        .then(|| Config::load(&config_file).ok())
        .flatten();
    let (authkey, _) = leviculum_std::daemon_rpc::resolve_authkey(config_dir, config.as_ref())
        .inspect_err(|error| tracing::info!("RPC path_table authkey failed: {error}"))?;
    // Every RPC call loquax makes is this one, and each says how it went: a
    // shared instance that logs "digest received was wrong" for a client
    // can then be checked against this line (leviculum 1002).
    let table = leviculum_std::rpc_query(instance, &authkey, "path_table")
        .await
        .map_err(|error| error.to_string())
        .inspect_err(|error| {
            tracing::info!("RPC path_table instance={instance} failed: {error}")
        })?;
    let routes = mailbox_state::routes_from_path_table(&table);
    tracing::info!(
        "RPC path_table instance={instance} ok entries={}",
        routes.len()
    );
    Ok(routes)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    // Once, before any subcommand resolves the state directory: a first
    // start after the rename must find the address it had as lnmsg.
    match identity::migrate_legacy_home() {
        Ok(identity::Migration::NotNeeded) => {}
        Ok(identity::Migration::Moved { from, to }) => {
            eprintln!("loquax: moved {} to {}", from.display(), to.display());
        }
        Err(error) => {
            eprintln!("loquax: {error}");
            return ExitCode::from(EXIT_FAILURE);
        }
    }
    // `LEVICULUM_EVENT_LOG=<path>` turns the structured events into an
    // append-only file in the documented format; without it this is the plain
    // fmt subscriber and the events cost a filtered-out call. `warn` rather
    // than `info` as the default: a successful run says nothing on any stream,
    // so an `info`-level line on stderr would be the only thing a cron job
    // ever saw from a send that worked. Whoever wants the detail asks for the
    // event log, which is where the message id lives.
    #[cfg(feature = "tui")]
    if args.daemon {
        return run_daemon(args.tui).await;
    }
    let command = match args.command {
        #[cfg(feature = "gui")]
        Some(SubCommand::Gui(gui)) => return run_gui(gui).await,
        #[cfg(feature = "tui")]
        Some(SubCommand::Tui(tui)) => return run_tui(tui).await,
        #[cfg(feature = "tui")]
        None => return run_tui(args.tui).await,
        #[cfg(not(feature = "tui"))]
        None => {
            eprintln!(
                "loquax: the messenger needs the `tui` feature, which this build lacks; \
                 see loquax --help for the commands it has"
            );
            return ExitCode::from(EXIT_USAGE);
        }
        Some(command) => command,
    };
    leviculum_std::event_log::install_global_subscriber("warn");

    match command {
        SubCommand::Send(send) => run(send).await,
        SubCommand::Fetch(fetch) => run_fetch_command(fetch).await,
        SubCommand::Address => run_address(),
        #[cfg(feature = "tui")]
        SubCommand::Home(home) => run_home(home).await,
        // Dispatched above, before the subscriber writes to stderr.
        #[cfg(feature = "tui")]
        SubCommand::Tui(_) => ExitCode::SUCCESS,
        #[cfg(feature = "gui")]
        SubCommand::Gui(_) => ExitCode::SUCCESS,
    }
}

/// `loquax gui`: parse what the shell needs, then hand over.
#[cfg(feature = "gui")]
async fn run_gui(args: GuiArgs) -> ExitCode {
    let sync_interval = match args
        .sync_interval
        .as_deref()
        .map(loquax::sync::parse_interval)
    {
        None => None,
        Some(Ok(bound)) => Some(bound),
        Some(Err(error)) => return usage(format!("--sync-interval {error}")),
    };
    gui::run(shell::ShellOptions {
        sync_interval,
        to: Vec::new(),
        pn: args.pn,
        instance: args.instance,
        config: args.config,
        timeout: args.timeout,
        ascii: false,
        no_mouse: false,
        scripted: false,
        log_file: "gui.log",
    })
    .await
}

/// `loquax` (or `loquax tui`): parse what the shell needs, then hand over.
#[cfg(feature = "tui")]
async fn run_tui(args: TuiArgs) -> ExitCode {
    let mut to = Vec::new();
    for text in &args.to {
        match address::parse(text) {
            Ok(peer) => to.push(peer),
            Err(error) => return usage(format!("--to {}: {error}", text.trim())),
        }
    }
    let size = args
        .size
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .filter(|&(w, h): &(u16, u16)| w >= 20 && h >= 8);
    let Some(size) = size else {
        return usage(format!("--size {}: want WxH, at least 20x8", args.size));
    };
    let sync_interval = match args
        .sync_interval
        .as_deref()
        .map(loquax::sync::parse_interval)
    {
        None => None,
        Some(Ok(bound)) => Some(bound),
        Some(Err(error)) => return usage(format!("--sync-interval {error}")),
    };
    tui::run(tui::TuiOptions {
        sync_interval,
        to,
        pn: args.pn,
        instance: args.instance,
        config: args.config,
        timeout: args.timeout,
        ascii: args.ascii,
        no_color: args.no_color,
        color: args.color.into(),
        theme: args.theme.map(Into::into),
        no_mouse: args.no_mouse,
        graphics: args.graphics,
        script: args.script,
        frames: args.frames,
        size,
    })
    .await
}

/// `loquax address`: print our own LXMF address. No daemon involved — the
/// address is a pure function of the persistent identity, minted on first
/// use, so this is also how a fresh machine learns (and creates) the address
/// a correspondent should write down.
fn run_address() -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    // A client's address is its home node's.
    match loquax::home::client::load_client(&home) {
        Ok(Some(public_key)) => {
            return match loquax::home::lxmf_address(&public_key) {
                Ok(address) => {
                    println!("{}", address::to_hex(&address));
                    ExitCode::SUCCESS
                }
                Err(error) => failure(error, "no-identity"),
            };
        }
        Ok(None) => {}
        Err(error) => return failure(error, "no-home"),
    }
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    match loquax::engine::delivery_address(&identity) {
        Ok(address) => {
            println!("{}", address::to_hex(&address));
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "no-identity"),
    }
}

async fn run(args: SendArgs) -> ExitCode {
    let destination = match address::parse(&args.address) {
        Ok(destination) => destination,
        Err(error) => return usage(format!("{}: {error}", args.address.trim())),
    };
    // Before the body is read: an unusable name is an argument error, and a
    // script piping into us should hear about it without first having its
    // stdin consumed.
    let display_name = match display_name::from_process(args.from.as_deref()) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    events::sender(&display_name.name, display_name.source.as_str());
    let body = match body::resolve(args.body.as_deref(), &mut std::io::stdin().lock()) {
        Ok(body) => body,
        Err(error) => return usage(error),
    };
    let title = args.title.unwrap_or_default().into_bytes();

    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    if let Some(refused) = refuse_on_client(&home) {
        return refused;
    }
    // Resolved before attaching for the same reason the display name is:
    // a bad --pn or a corrupt config line is an argument-shaped problem and
    // should not cost a daemon connection first.
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match attach(AttachConfig {
        instance: instance.clone(),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: Terminal::new(home.clone()),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return failure(error, "no-daemon"),
    };

    let mut options = SendOptions::new(instance.clone(), Duration::from_secs(args.timeout));
    let mut effects = Terminal::new(home.clone());
    let clock = Tokio::new();
    // `--via auto` reads the path table only if its direct leg fails.
    if args.via == ViaArg::Propagated {
        options.mailbox =
            mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()).await;
    }
    let request = |via: Via| SendRequest {
        destination,
        title: title.clone(),
        body: body.clone(),
        fields: Vec::new(),
        via,
    };

    let code = match args.via {
        ViaArg::Direct => {
            let outcome =
                run_send(&mut attached.outbox, request(Via::Direct), &options, &clock).await;
            match outcome {
                Ok(queued) => {
                    events::done(Some(&queued.message_id), "queued", 0);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("loquax: {error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ViaArg::Propagated => {
            let outcome = run_send_propagated(
                &mut attached.outbox,
                request(Via::Propagated),
                preferred_pn,
                pn_source,
                None,
                &options,
                &mut effects,
                &clock,
            )
            .await;
            match outcome {
                Ok(queued) => {
                    events::via("propagated", "requested");
                    events::done(Some(&queued.message_id), "propagated", EXIT_PROPAGATED);
                    ExitCode::from(EXIT_PROPAGATED)
                }
                Err(error) => {
                    eprintln!("loquax: {error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
        ViaArg::Auto => {
            let outcome = run_send_auto(
                &mut attached.outbox,
                request(Via::Link),
                preferred_pn,
                pn_source,
                &mut options,
                || mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()),
                &mut effects,
                &clock,
            )
            .await;
            match outcome {
                Ok(AutoSent::Direct(queued)) => {
                    events::done(Some(&queued.message_id), "queued", 0);
                    ExitCode::SUCCESS
                }
                Ok(AutoSent::Propagated(queued)) => {
                    events::done(Some(&queued.message_id), "propagated", EXIT_PROPAGATED);
                    ExitCode::from(EXIT_PROPAGATED)
                }
                Err(AutoError::Direct(direct_error)) => {
                    eprintln!("loquax: {direct_error}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
                Err(AutoError::Both { direct, fallback }) => {
                    eprintln!("loquax: direct: {direct}\nloquax: propagated: {fallback}");
                    events::done(None, "failed", EXIT_FAILURE);
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
    };
    attached.stop().await;
    code
}

async fn run_fetch_command(args: FetchArgs) -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    match loquax::home::client::load_client(&home) {
        Ok(None) => {}
        Ok(Some(public_key)) => return run_client_fetch(home, public_key, args).await,
        Err(error) => return failure(error, "no-home"),
    }
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    let mut seen = match SeenStore::load(&home) {
        Ok(seen) => seen,
        Err(error) => return failure(format!("seen store: {error}"), "no-home"),
    };

    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));

    // The display name travels in the delivery announce a fetch also sends;
    // there is no --from here because a fetch is not authored.
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };

    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match attach(AttachConfig {
        instance: instance.clone(),
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        notices: Terminal::new(home.clone()),
    })
    .await
    {
        Ok(attached) => attached,
        Err(error) => return failure(error, "no-daemon"),
    };

    let mut options = FetchOptions::new(instance.clone(), Duration::from_secs(args.timeout));
    let mut effects = Terminal::new(home.clone());
    options.mailbox = mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()).await;
    // `Terminal::print` deliberately does NOT hold `stdout().lock()` across
    // the run: the engine's driver thread logs through tracing, and with
    // `RUST_LOG=debug` (how the conformance rig runs every tool) the fmt
    // subscriber writes to this same stdout. A lock held across the whole
    // fetch deadlocked the first engine tick against it for the entire run.
    // `run_fetch` renders each message into one buffer and hands it over as
    // a single write, so output stays whole without anyone holding the
    // stream.
    let outcome = run_fetch(
        &mut attached.outbox,
        preferred_pn,
        pn_source,
        &options,
        &mut effects,
        &mut seen,
        &Tokio::new(),
    )
    .await;

    let code = match outcome {
        Ok(fetched) => {
            events::done(None, if fetched.new > 0 { "fetched" } else { "empty" }, 0);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("loquax: {error}");
            match error {
                FetchError::NoPath(attempts) => {
                    events::done_without_path(EXIT_FAILURE, attempts.requests, attempts.waited_ms)
                }
                _ => events::done(None, "failed", EXIT_FAILURE),
            }
            ExitCode::from(EXIT_FAILURE)
        }
    };
    attached.stop().await;
    code
}

/// A client of a home node sends nothing of its own yet: it holds no LXMF
/// identity, and one minted here would be a second address beside the home
/// node's, the very split the home node exists to end.
fn refuse_on_client(home: &Path) -> Option<ExitCode> {
    match loquax::home::client::load_client(home) {
        Ok(None) => None,
        Ok(Some(_)) => Some(failure(
            format!(
                "{}; open the messenger to read what the home node holds",
                loquax::engine::CLIENT_SENDS
            ),
            "client",
        )),
        Err(error) => Some(failure(error, "no-home")),
    }
}

/// `loquax --daemon`: the home node.
#[cfg(feature = "tui")]
async fn run_daemon(args: TuiArgs) -> ExitCode {
    // The journal under systemd: loquax's own lines (archived, admitted,
    // refused) and every warning. `RUST_LOG` replaces the filter.
    leviculum_std::event_log::install_global_subscriber("warn,loquax=info");
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    match loquax::home::client::load_client(&home) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return failure(
                "this device is a client of a home node, so it cannot be one; \
                 the home node runs where the identity is",
                "client",
            )
        }
        Err(error) => return failure(error, "no-home"),
    }
    let (preferred_pn, pn_source) = match resolve_pn(args.pn.as_deref(), &home) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let config = match loquax::config::load(&home) {
        Ok(config) => config,
        Err(error) => return usage(error.to_string()),
    };
    let sync_bound_ms = match args
        .sync_interval
        .as_deref()
        .map(loquax::sync::parse_interval)
    {
        None => config
            .sync_interval
            .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        Some(Ok(bound)) => bound,
        Some(Err(error)) => return usage(format!("--sync-interval {error}")),
    };
    let identity = match identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity,
        Err(error) => return failure(error, "no-identity"),
    };
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let archive_cap_mb = config
        .archive_cap_mb
        .unwrap_or(loquax::home::DEFAULT_ARCHIVE_CAP_MB);
    let mailbox: loquax::home::daemon::MailboxSource = {
        let (home, config_dir, instance) = (home.clone(), config_dir.clone(), instance.clone());
        Box::new(move || {
            let (home, config_dir, instance) = (home.clone(), config_dir.clone(), instance.clone());
            Box::pin(async move {
                mailbox_run(&home, &config_dir, &instance, preferred_pn, Vec::new()).await
            })
        })
    };
    let outcome = loquax::home::daemon::run(loquax::home::daemon::DaemonOptions {
        home,
        instance,
        storage_dir,
        identity,
        display_name: display_name.name.into_bytes(),
        archive_cap_bytes: archive_cap_mb.saturating_mul(1 << 20),
        preferred_pn,
        pn_source,
        sync_bound_ms,
        fetch_budget: Duration::from_secs(args.timeout.saturating_mul(2)),
        mailbox,
    })
    .await;
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failure(error, "failed"),
    }
}

/// `loquax home ...`.
#[cfg(feature = "tui")]
async fn run_home(command: HomeCommand) -> ExitCode {
    let home = match identity::home_dir() {
        Ok(home) => home,
        Err(error) => return failure(error, "no-home"),
    };
    let client = match loquax::home::client::load_client(&home) {
        Ok(client) => client,
        Err(error) => return failure(error, "no-home"),
    };
    let on_home_node = |store: &dyn Fn(&mut loquax::store::Store) -> ExitCode| {
        if client.is_some() {
            return failure(
                "this device is a client of a home node; run this on the home node",
                "client",
            );
        }
        let identity = match identity::load_or_create(&home.join("identity")) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
        let address = match loquax::engine::delivery_address(&identity) {
            Ok(address) => address,
            Err(error) => return failure(error, "no-identity"),
        };
        match loquax::store::Store::open_in(&home, address) {
            Ok(mut opened) => store(&mut opened),
            Err(error) => failure(error, "no-store"),
        }
    };
    let now = leviculum_std::local_time::unix_now_secs();
    match command {
        HomeCommand::Admit { name } => on_home_node(&|store| {
            let name = match &name {
                Some(name) => name.trim().to_string(),
                None => match store.devices() {
                    Ok(devices) => format!("device-{}", devices.len() + 1),
                    Err(error) => return failure(error, "no-store"),
                },
            };
            if name.is_empty() {
                return usage("--name may not be empty");
            }
            let mut token = [0u8; 16];
            rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut token);
            if let Err(error) = store.issue_token(&token, &name, now) {
                return failure(error, "no-store");
            }
            let public_key = match identity::load_or_create(&home.join("identity")) {
                Ok(identity) => identity.public_key_bytes(),
                Err(error) => return failure(error, "no-identity"),
            };
            let code = loquax::home::code::HomeCode { public_key, token }.encode();
            println!("Admission code for {name:?}, good for one device, once:\n\n{code}\n");
            println!("On that device: loquax home join {code}");
            ExitCode::SUCCESS
        }),
        HomeCommand::Devices => on_home_node(&|store| match store.devices() {
            Ok(devices) => {
                for device in devices {
                    let at = loquax::when::WallTime {
                        unix_secs: device.admitted_at,
                        offset_secs: leviculum_std::local_time::utc_offset_secs(device.admitted_at),
                    }
                    .civil();
                    println!(
                        "{}  {}  admitted {:04}-{:02}-{:02} {:02}:{:02}",
                        address::to_hex(&device.device),
                        device.name,
                        at.year,
                        at.month,
                        at.day,
                        at.hour,
                        at.minute
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => failure(error, "no-store"),
        }),
        HomeCommand::Revoke { device } => on_home_node(&|store| match store.revoke(&device) {
            Ok(loquax::store::Revoked::One(gone)) => {
                println!("revoked {}  {}", address::to_hex(&gone.device), gone.name);
                ExitCode::SUCCESS
            }
            Ok(loquax::store::Revoked::None) => {
                failure(format!("no admitted device is called {device:?}"), "failed")
            }
            Ok(loquax::store::Revoked::Ambiguous(matched)) => {
                let names: Vec<String> = matched
                    .iter()
                    .map(|d| format!("{} {}", address::to_hex(&d.device), d.name))
                    .collect();
                usage(format!(
                    "{device:?} names {} devices ({}); give the hash",
                    matched.len(),
                    names.join(", ")
                ))
            }
            Err(error) => failure(error, "no-store"),
        }),
        HomeCommand::Join(args) => run_join(home, client, args).await,
    }
}

/// `loquax home join <code>`.
#[cfg(feature = "tui")]
async fn run_join(home: PathBuf, client: Option<[u8; 64]>, args: JoinArgs) -> ExitCode {
    let code = match loquax::home::code::HomeCode::parse(&args.code) {
        Ok(code) => code,
        Err(error) => return usage(error),
    };
    if client.is_some_and(|key| key != code.public_key) {
        return failure(
            "this device is already the client of another home node",
            "client",
        );
    }
    let address = match loquax::home::lxmf_address(&code.public_key) {
        Ok(address) => address,
        Err(error) => return usage(error),
    };
    let device =
        match identity::load_or_create(&home.join(loquax::home::client::DEVICE_IDENTITY_FILE)) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
    let display_name = match display_name::from_process(None) {
        Ok(resolved) => resolved,
        Err(error) => return usage(error),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let attached = match loquax::engine::attach_as(
        AttachConfig {
            instance,
            storage_dir,
            identity: device.clone(),
            display_name: Vec::new(),
            notices: Terminal::new(home.clone()),
        },
        loquax::engine::Role::Client { address },
    )
    .await
    {
        Ok(attached) => attached,
        Err(error) => return failure(error, "no-daemon"),
    };
    let node = attached.node();
    let joined = loquax::home::client::join(
        &node,
        &code.public_key,
        &device,
        &code.token,
        &display_name.name,
        Duration::from_secs(args.timeout),
    )
    .await;
    drop(node);
    attached.stop().await;
    match joined {
        Ok(()) => match loquax::home::client::save_client(&home, &code.public_key) {
            Ok(()) => {
                println!(
                    "joined: this device reads the mail of {}",
                    address::to_hex(&address)
                );
                ExitCode::SUCCESS
            }
            Err(error) => failure(error, "no-home"),
        },
        Err(error) => failure(error, "failed"),
    }
}

/// `loquax fetch` on a client of a home node: pull what the home node holds
/// after this device's cursor and print what is new.
async fn run_client_fetch(home: PathBuf, public_key: [u8; 64], args: FetchArgs) -> ExitCode {
    let address = match loquax::home::lxmf_address(&public_key) {
        Ok(address) => address,
        Err(error) => return failure(error, "no-identity"),
    };
    let device =
        match identity::load_or_create(&home.join(loquax::home::client::DEVICE_IDENTITY_FILE)) {
            Ok(identity) => identity,
            Err(error) => return failure(error, "no-identity"),
        };
    let mut seen = match SeenStore::load(&home) {
        Ok(seen) => seen,
        Err(error) => return failure(format!("seen store: {error}"), "no-home"),
    };
    let config_dir = args.config.unwrap_or_else(Config::default_config_dir);
    let instance = args.instance.unwrap_or_else(|| instance_name(&config_dir));
    let storage_dir = match storage_dir(&home) {
        Ok(dir) => dir,
        Err(error) => return failure(error, "no-daemon"),
    };
    let mut attached = match loquax::engine::attach_as(
        AttachConfig {
            instance,
            storage_dir,
            identity: device.clone(),
            display_name: Vec::new(),
            notices: Terminal::new(home.clone()),
        },
        loquax::engine::Role::Client { address },
    )
    .await
    {
        Ok(attached) => attached,
        Err(error) => return failure(error, "no-daemon"),
    };
    let node = attached.node();
    let mut effects = Terminal::new(home.clone());
    let outcome = loquax::home::client::fetch(
        &mut attached.outbox,
        &node,
        &home,
        &public_key,
        &device,
        Duration::from_secs(args.timeout),
        &mut effects,
        &mut seen,
    )
    .await;
    drop(node);
    attached.stop().await;
    match outcome {
        Ok(new) => {
            events::done(None, if new > 0 { "fetched" } else { "empty" }, 0);
            ExitCode::SUCCESS
        }
        Err(error) => failure(error, "failed"),
    }
}
