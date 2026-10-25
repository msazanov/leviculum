//! The engine's shell: the driver it runs inside, the channels to the
//! frontend, the mining thread and its runtime.
//!
//! [`Engine`] is the core and holds its own queues; what this module adds is
//! everything that needs a process: a [`CoreProcessor`] for the driver's
//! `StdNodeCore` that moves those queues onto `mpsc` channels around every
//! hook, the [`InProcessOutbox`] the frontend holds, [`attach`], and the
//! stamp worker thread that runs [`super::mine`] on a current-thread tokio
//! runtime with `OsRng`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use leviculum_core::identity::Identity;
use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_std::driver::{CoreProcessor, ReticulumNodeBuilder, StdNodeCore};
use leviculum_std::ReticulumNode;

use super::{Engine, StampAnswer, StampJob};
use crate::clock;
use crate::effects::{Effects, Terminal};
use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};

/// The engine as the driver sees it, with the frontend's and the miner's
/// ends of its queues.
struct Processor {
    engine: Engine,
    commands: Receiver<Command>,
    events: Sender<OutboxEvent>,
    stamps: Sender<StampJob>,
    stamp_answers: Receiver<StampAnswer>,
}

impl Processor {
    /// Move what the frontend and the miner sent into the engine's queues.
    /// Neither blocks; a gone sender has simply nothing more to send.
    fn take_inputs(&mut self) {
        // An error is either empty or disconnected, and a disconnected
        // frontend means the node is being torn down: both end the drain.
        while let Ok(command) = self.commands.try_recv() {
            self.engine.submit(command);
        }
        while let Ok(answer) = self.stamp_answers.try_recv() {
            self.engine.stamp_answered(answer);
        }
    }

    /// Hand on what the hook produced.
    fn flush(&mut self) {
        while let Some(event) = self.engine.next_event() {
            // A dropped receiver means the frontend has already finished;
            // the engine keeps running until the node stops, and has nothing
            // to say about it.
            let _ = self.events.send(event);
        }
        while let Some(job) = self.engine.next_stamp_job() {
            if let Err(std::sync::mpsc::SendError(job)) = self.stamps.send(job) {
                self.engine.stamp_undelivered(&job);
            }
        }
    }
}

impl CoreProcessor for Processor {
    fn on_event(&mut self, core: &mut StdNodeCore, event: &NodeEvent) -> TickOutput {
        let out = self.engine.on_event(core, event);
        self.flush();
        out
    }

    fn on_tick(&mut self, core: &mut StdNodeCore, now_ms: u64) -> TickOutput {
        self.take_inputs();
        let out = self.engine.on_tick(core, now_ms);
        self.flush();
        out
    }
}

/// The frontend's end of the in-process engine.
pub struct InProcessOutbox {
    commands: Sender<Command>,
    events: Receiver<OutboxEvent>,
}

impl Outbox for InProcessOutbox {
    fn submit(&self, command: Command) -> Result<(), OutboxGone> {
        self.commands.send(command).map_err(|_| OutboxGone)
    }

    fn try_next_event(&mut self) -> Result<Option<OutboxEvent>, OutboxGone> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(OutboxGone),
        }
    }
}

/// What attaching to a shared instance needs to know. `N` is where the stamp
/// worker's progress lines go: the CLI's [`Terminal`] writes them to stderr,
/// a full-screen frontend has to catch them before they land on its screen.
pub struct AttachConfig<N = Terminal> {
    /// The shared instance to join, i.e. which daemon.
    pub instance: String,
    /// Where this client keeps its node state (learned identities, paths).
    ///
    /// Deliberately not the daemon's storage directory: `lncp` and `lnstatus`
    /// share that safely because a client with `enable_transport(false)`
    /// writes no paths, announces or packet hashes — this one registers a
    /// destination and learns identities, so it gets its own. The caller
    /// creates it: a directory is the shell's, and `main` names the path in
    /// the error when it cannot.
    pub storage_dir: PathBuf,
    /// The persistent identity whose delivery destination is our address.
    pub identity: Identity,
    /// The name recipients see as the sender: it goes out in the delivery
    /// announce `Engine::announce` sends, and a reference client shows it
    /// next to every message from us. Resolved by
    /// [`crate::display_name::from_process`], which never yields an empty one.
    pub display_name: Vec<u8>,
    /// Where the stamp worker's progress lines go.
    pub notices: N,
}

/// Why attaching failed.
#[derive(Debug)]
pub enum AttachError {
    /// No daemon answered. The message names the instance, because the usual
    /// cause is the right daemon running under a different name.
    ///
    /// Both the build and the start of a shared-instance client fail this way,
    /// and they are one error to a user: the IPC socket is the only thing
    /// either step reaches for. Which of the two noticed is in `detail`.
    NoDaemon { instance: String, detail: String },
}

impl core::fmt::Display for AttachError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDaemon { instance, detail } => write!(
                f,
                "could not join the Reticulum shared instance named '{instance}'.\n  \
                 lnmsg talks to a daemon that is already running; it does not start a \
                 Reticulum stack of its own.\n  \
                 Start one with `lnsd` (or Python's `rnsd`), or name another instance \
                 with --instance / --config.\n  \
                 The stack said: {detail}"
            ),
        }
    }
}

impl core::error::Error for AttachError {}

/// A running client node and the outbox that talks to its engine.
pub struct Attached {
    node: ReticulumNode,
    /// The frontend's end of the seam.
    pub outbox: InProcessOutbox,
}

impl Attached {
    /// Stop the node. Best effort: the send outcome is already decided by the
    /// time anything calls this.
    pub async fn stop(mut self) {
        let _ = self.node.stop().await;
    }
}

/// Attach to a running shared instance with an LXMF engine installed.
pub async fn attach<N: Effects + Send + 'static>(
    config: AttachConfig<N>,
) -> Result<Attached, AttachError> {
    let (commands_tx, commands_rx) = std::sync::mpsc::channel::<Command>();
    let (events_tx, events_rx) = std::sync::mpsc::channel::<OutboxEvent>();
    let (stamps_tx, stamps_rx) = std::sync::mpsc::channel::<StampJob>();
    let (answers_tx, answers_rx) = std::sync::mpsc::channel::<StampAnswer>();

    // Proof-of-work, off the core lock and off the runtime's workers: mining
    // is pure CPU that can run for minutes at a cost the *peer* chooses. Its
    // own thread with its own current-thread runtime, exactly as
    // `leviculum-lxmf-node` does it (`leviculum-lxmf-node/src/main.rs:224-255`),
    // because the generator is async. Dormant unless a peer advertises a stamp
    // cost, which most do not.
    let notices = config.notices;
    std::thread::spawn(move || run_stamp_worker(&stamps_rx, &answers_tx, notices));

    let processor = Processor {
        engine: Engine::new(config.identity, config.display_name),
        commands: commands_rx,
        events: events_tx,
        stamps: stamps_tx,
        stamp_answers: answers_rx,
    };

    let mut node = ReticulumNodeBuilder::new()
        .enable_transport(false)
        .connect_to_shared_instance(&config.instance)
        .storage_path(config.storage_dir)
        .core_processor(processor)
        // The engine consumes events on the processor tap; nothing here ever
        // takes the driver's application receiver, so enabling it would only
        // fill a queue with no reader (Codeberg #419).
        .without_events()
        .build()
        .await
        .map_err(|e| AttachError::NoDaemon {
            instance: config.instance.clone(),
            detail: e.to_string(),
        })?;
    node.start().await.map_err(|e| AttachError::NoDaemon {
        instance: config.instance.clone(),
        detail: e.to_string(),
    })?;

    Ok(Attached {
        node,
        outbox: InProcessOutbox {
            commands: commands_tx,
            events: events_rx,
        },
    })
}

/// The terminal's miner: one job at a time, each on tokio's clock with a
/// fresh `OsRng` draw source.
fn run_stamp_worker(
    jobs: &Receiver<StampJob>,
    answers: &Sender<StampAnswer>,
    mut notices: impl Effects,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let clock = clock::Tokio::new();
    while let Ok(job) = jobs.recv() {
        let answer = runtime.block_on(super::mine(job, rand_core::OsRng, &clock, &mut notices));
        if answers.send(answer).is_err() {
            return;
        }
    }
}
