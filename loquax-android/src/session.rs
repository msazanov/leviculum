//! The app's session: a leaf node with one TCP interface to the home node
//! (`loquax::engine::attach_leaf`), the client engine behind the Outbox
//! seam, the store, and loquax's screen model, which decides what a
//! message, a send, a mark and a pull do to the conversations exactly as it
//! does in the terminal. The model's effects run here on a tokio runtime the
//! session owns, through the client runners the terminal's shell runs too
//! (`loquax::home::runner`): sends through `/send` one at a time, pulls,
//! marks.
//!
//! Java never touches the engine. It calls [`Session::request`] and
//! [`Session::query`] from any thread, which take the session's lock
//! for as long as a fold or a read takes, and waits for
//! [`Session::next_event`] on a thread of its own.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use leviculum_lxmf::Field;
use leviculum_ui::commands::Command as PaletteCommand;
use leviculum_ui::delivery::DeliveryState;
use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::engine::{attach_leaf, Attached, LeafConfig, Role};
use loquax::fanout::Fanout;
use loquax::home::runner::{self, HomeClient};
use loquax::outbox::{Command, Outbox, OutboxEvent};
use loquax::store::Store;
use loquax::ui::{reactions, update, Message, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;

use crate::schema::{self, Event, VERSION};

/// The file under the app's directory that keeps the home node's address.
pub const SERVER_FILE: &str = "home-server";
/// How long a join, a send or a mark may take; a pull gets twice this.
pub const BUDGET: Duration = Duration::from_secs(60);
/// How often the session takes what the engine said and runs the clock:
/// the terminal's pump interval.
const PUMP: Duration = Duration::from_millis(100);
/// How long a send runner waits for its bubble to be named.
const SETTLE: Duration = Duration::from_secs(5);
/// How many pumps between two looks at the TCP interface: a second.
const LINK_EVERY: u32 = 10;

/// Why the session did not start, and what the join screen has to ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError {
    pub need: Option<&'static str>,
    pub text: String,
}

impl StartError {
    fn other(text: impl Into<String>) -> Self {
        Self {
            need: None,
            text: text.into(),
        }
    }
}

/// The compose field the model needs a type for. Nobody types into it: the
/// app's own text field sends with [`UiEvent::Write`].
#[derive(Debug, Default, Clone)]
pub struct Line(String);

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.0
    }
    fn cursor(&self) -> usize {
        self.0.chars().count()
    }
    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
    fn newline(&mut self) {
        self.0.push('\n');
    }
    fn set_cursor(&mut self, _at: usize) {}
}

/// A runner's end, folded by the pump.
enum Finished {
    Send(UiEvent),
    Pull {
        result: Result<usize, String>,
        cursor: Option<u64>,
    },
}

/// Everything the lock guards.
struct Inner {
    model: UiModel<Line>,
    store: Store,
    attached: Attached,
    fanout: Fanout,
    client: HomeClient,
    home: PathBuf,
    offset: i32,
    sends: VecDeque<([u8; 16], String, Vec<Field>)>,
    sending: bool,
    pull_owed: bool,
    pulling: bool,
    finished_tx: Sender<Finished>,
    finished_rx: Receiver<Finished>,
    events: Sender<Event>,
    runtime: tokio::runtime::Handle,
    origin: std::time::Instant,
    /// Something the screen shows changed since the last `changed`.
    dirty: bool,
    gone: bool,
    /// Whether the TCP interface to the home node was up at the last look,
    /// none before the first, and the pumps since that look.
    link: Option<bool>,
    link_age: u32,
}

/// A running session.
pub struct Session {
    inner: Arc<Mutex<Inner>>,
    events: Mutex<Receiver<Event>>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    address: [u8; 16],
    server: String,
}

impl Session {
    /// Start: on a phone that is no client yet, join with the code first.
    /// Blocks for as long as the join takes, up to [`BUDGET`].
    /// A code with the address behind it (`lqxh1...@host:port`) brings the
    /// server along; a server given beside it wins.
    pub fn start(start: &schema::Start) -> Result<Self, StartError> {
        let home = PathBuf::from(&start.dir);
        let client = loquax::home::client::load_client(&home).map_err(StartError::other)?;
        let mut carried = None;
        let code = match (&client, start.code.as_deref().map(str::trim)) {
            (_, Some(code)) if !code.is_empty() => {
                let (code, server) = loquax::home::code::HomeCode::parse_with_server(code)
                    .map_err(|error| StartError {
                        need: Some("code"),
                        text: format!("That is not a home code: {error}"),
                    })?;
                carried = server;
                Some(code)
            }
            (Some(_), _) => None,
            (None, _) => {
                return Err(StartError {
                    need: Some("code"),
                    text: "This phone is not joined to a home node yet. Paste the code \
                           `loquax home admit` printed on the home node."
                        .to_string(),
                })
            }
        };
        if let (Some(key), Some(code)) = (&client, &code) {
            if *key != code.public_key {
                return Err(StartError {
                    need: None,
                    text: "This phone is already joined to another home node.".to_string(),
                });
            }
        }
        let server = match start.server.as_deref().map(str::trim) {
            Some(server) if !server.is_empty() => server.to_string(),
            _ => carried.or_else(|| load_server(&home)).ok_or(StartError {
                need: Some("server"),
                text: "Type the home node's address, host:port of its lnsd TCP server.".to_string(),
            })?,
        };
        loquax::engine::parse_server(&server).map_err(|text| StartError {
            need: Some("server"),
            text,
        })?;
        let public_key = match (&code, client) {
            (Some(code), _) => code.public_key,
            (None, Some(key)) => key,
            (None, None) => return Err(StartError::other("no home node")),
        };
        let address = loquax::home::lxmf_address(&public_key).map_err(StartError::other)?;
        let device = loquax::identity::load_or_create(
            &home.join(loquax::home::client::DEVICE_IDENTITY_FILE),
        )
        .map_err(|error| StartError::other(error.to_string()))?;
        let storage = home.join("storage");
        std::fs::create_dir_all(&storage)
            .map_err(|error| StartError::other(format!("{}: {error}", storage.display())))?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| StartError::other(format!("no runtime: {error}")))?;
        let attached = runtime.block_on(attach_leaf(
            LeafConfig {
                server: server.clone(),
                storage_dir: storage,
                identity: device.clone(),
                display_name: Vec::new(),
                notices: loquax::effects::Terminal::new(home.clone()),
            },
            Role::Client { address },
        ));
        let attached = attached.map_err(|error| StartError::other(error.to_string()))?;
        let client = HomeClient {
            node: attached.node(),
            public_key,
            device,
        };
        if let Some(code) = &code {
            let name = start.name.clone().unwrap_or_else(|| "phone".to_string());
            let joined = runtime.block_on(loquax::home::client::join(
                &client.node,
                &code.public_key,
                &client.device,
                &code.token,
                &name,
                BUDGET,
            ));
            if let Err(error) = joined {
                drop(client);
                runtime.block_on(attached.stop());
                let need = match error {
                    loquax::home::client::HomeError::NoPath => Some("server"),
                    loquax::home::client::HomeError::Refused(_) => Some("code"),
                    _ => None,
                };
                return Err(StartError {
                    need,
                    text: join_words(&error, &server),
                });
            }
            loquax::home::client::save_client(&home, &code.public_key)
                .map_err(StartError::other)?;
        }
        save_server(&home, &server).map_err(StartError::other)?;

        let (events_tx, events_rx) = channel();
        let inner = Inner::new(
            &home,
            address,
            attached,
            client,
            start.offset,
            events_tx,
            runtime.handle().clone(),
        )
        .map_err(StartError::other)?;
        let inner = Arc::new(Mutex::new(inner));
        let pumped = Arc::clone(&inner);
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(PUMP).await;
                let Ok(mut inner) = pumped.lock() else {
                    return;
                };
                if !inner.pump() {
                    return;
                }
            }
        });
        let session = Self {
            inner,
            events: Mutex::new(events_rx),
            runtime: Mutex::new(Some(runtime)),
            address,
            server,
        };
        // What the home node holds since the last run, on open.
        session.lock().apply(UiEvent::Run(PaletteCommand::Fetch));
        Ok(session)
    }

    /// The home node's LXMF address, this phone's own.
    pub fn address(&self) -> [u8; 16] {
        self.address
    }

    /// The server the session talks to.
    pub fn server(&self) -> &str {
        &self.server
    }

    /// Whether the TCP interface to the home node is up now.
    pub fn link(&self) -> bool {
        link_up(&self.lock().client.node)
    }

    /// Whether the session ended by itself: the engine is gone, and only a
    /// fresh start helps.
    pub fn is_over(&self) -> bool {
        self.lock().gone
    }

    /// Cut the session off its engine, as the engine's death does; the
    /// next pump finds it gone. For the facade test.
    #[doc(hidden)]
    pub fn simulate_engine_death(&self) {
        self.lock().attached.outbox.sever();
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic under the lock is caught at the JNI edge; what it left
        // is still the best state there is.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// What the user did.
    pub fn request(&self, request: schema::Request) -> Result<(), String> {
        let event = match request {
            schema::Request::Open { peer } => {
                let peer = parse_peer(&peer)?;
                let mut inner = self.lock();
                inner.apply(UiEvent::Open { peer });
                inner.apply(UiEvent::Run(PaletteCommand::Fetch));
                return Ok(());
            }
            schema::Request::Send { peer, body } => UiEvent::Write {
                peer: parse_peer(&peer)?,
                body,
            },
            schema::Request::React {
                peer,
                target,
                content,
            } => UiEvent::React {
                peer: parse_peer(&peer)?,
                target: parse_id(&target)?,
                content,
            },
            schema::Request::Close => UiEvent::Close,
            schema::Request::Pull => UiEvent::Run(PaletteCommand::Fetch),
        };
        self.lock().apply(event);
        Ok(())
    }

    /// What the screen wants to show, encoded.
    pub fn query(&self, query: schema::Query) -> Result<Vec<u8>, String> {
        let inner = self.lock();
        Ok(match query {
            schema::Query::Conversations => schema::encode(&inner.conversations()),
            schema::Query::Messages { peer } => schema::encode(&inner.messages(parse_peer(&peer)?)),
        })
    }

    /// The next event, waiting at most `wait`.
    pub fn next_event(&self, wait: Duration) -> Option<Event> {
        let events = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match events.recv_timeout(wait) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Event::Stopped {
                v: VERSION,
                detail: String::new(),
            }),
        }
    }

    /// Stop the node and the runtime. Marks still running get a moment.
    pub fn stop(&self) {
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(runtime) = runtime else {
            return;
        };
        {
            let mut inner = self.lock();
            inner.gone = true;
            let _ = inner.events.send(Event::Stopped {
                v: VERSION,
                detail: String::new(),
            });
        }
        runtime.shutdown_timeout(Duration::from_secs(2));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Inner {
    fn new(
        home: &Path,
        address: [u8; 16],
        attached: Attached,
        client: HomeClient,
        offset: i32,
        events: Sender<Event>,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let store = Store::open_in(home, address).map_err(|error| error.to_string())?;
        let config = loquax::config::load(home).map_err(|error| error.to_string())?;
        let contacts = loquax::names::load_contacts(home).map_err(|error| error.to_string())?;
        let mut model = UiModel::<Line>::new(&[]);
        model.home_client = true;
        model.list_apart = true;
        model.names = loquax::names::Names::new(contacts);
        for (peer, name) in store.names().map_err(|error| error.to_string())? {
            model.names.remember(peer, &name);
        }
        for (message_id, source, packed) in store.unverified().map_err(|e| e.to_string())? {
            let verify = Command::Verify {
                message_id,
                source,
                packed,
            };
            if attached.outbox.submit(verify).is_err() {
                break;
            }
        }
        let now = wall_now(offset);
        update(&mut model, UiEvent::Clock(now));
        model.restore(store.restore().map_err(|error| error.to_string())?);
        // The phone starts on the list, with nothing open: the model's
        // terminal habit of opening the first conversation would read what
        // arrives there before anyone looked.
        update(&mut model, UiEvent::Close);
        model.activity = model
            .activity
            .max(store.last_key().map_err(|error| error.to_string())?);
        // The pull schedule of the terminal: while the app runs.
        model.sync = Some(loquax::sync::Schedule::new(
            config
                .sync_interval
                .unwrap_or(Some(loquax::sync::DEFAULT_BOUND_MS)),
        ));
        let (finished_tx, finished_rx) = channel();
        Ok(Self {
            model,
            store,
            attached,
            fanout: Fanout::default(),
            client,
            home: home.to_path_buf(),
            offset,
            sends: VecDeque::new(),
            sending: false,
            pull_owed: false,
            pulling: false,
            finished_tx,
            finished_rx,
            events,
            runtime,
            origin: std::time::Instant::now(),
            dirty: true,
            gone: false,
            link: None,
            // The first pump looks, and says what it saw.
            link_age: LINK_EVERY,
        })
    }

    /// One turn of the pump: what the engine said, what the runners
    /// finished, the clock. `false` once the session is over.
    fn pump(&mut self) -> bool {
        if self.gone {
            return false;
        }
        match self.fanout.pump(&mut self.attached.outbox) {
            Ok(events) => {
                for event in events {
                    if runner::already_held(&self.store, &event) {
                        continue;
                    }
                    runner::keep_signature(&mut self.store, &event);
                    let named = match &event {
                        OutboxEvent::Announced {
                            destination,
                            name: Some(_),
                        } => Some(*destination),
                        _ => None,
                    };
                    self.apply(UiEvent::Engine(event));
                    if let Some(peer) = named {
                        runner::keep_name(
                            &mut self.store,
                            &self.model.names,
                            self.model.address,
                            peer,
                        );
                    }
                }
            }
            Err(gone) => {
                self.gone = true;
                let _ = self.events.send(Event::Stopped {
                    v: VERSION,
                    detail: gone.to_string(),
                });
                return false;
            }
        }
        while let Ok(finished) = self.finished_rx.try_recv() {
            match finished {
                Finished::Send(event) => {
                    self.sending = false;
                    self.apply(event);
                }
                Finished::Pull { result, cursor } => self.pulled(result, cursor),
            }
        }
        self.look_at_link();
        let elapsed = u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.quiet(UiEvent::Clock(wall_now(self.offset)));
        self.quiet(UiEvent::Tick);
        self.apply(UiEvent::Elapsed(elapsed));
        if std::mem::take(&mut self.dirty) {
            let _ = self.events.send(Event::Changed { v: VERSION });
        }
        true
    }

    /// Once a second: whether the TCP interface is up, told when it
    /// changed and on the first look.
    fn look_at_link(&mut self) {
        self.link_age += 1;
        if self.link_age < LINK_EVERY {
            return;
        }
        self.link_age = 0;
        let up = link_up(&self.client.node);
        if up && self.link == Some(false) {
            self.ask_for_home_path();
        }
        if self.link != Some(up) {
            self.link = Some(up);
            let _ = self.events.send(Event::Link { v: VERSION, up });
        }
    }

    /// The interface came back: ask for the home destination's path. The
    /// home node never announces that destination, so a transport node that
    /// restarted (the box rebooted) knows no path to it until someone asks,
    /// while this leaf still holds the path from before and would send its
    /// link requests into the void until they time out. The path request
    /// makes the transport ask the home node, which answers it.
    fn ask_for_home_path(&self) {
        let Ok(hash) = loquax::home::destination_hash(&self.client.public_key) else {
            return;
        };
        let node = std::sync::Arc::clone(&self.client.node);
        self.runtime.spawn(async move {
            let destination = leviculum_core::DestinationHash::new(hash);
            if let Err(error) = node.request_path(&destination).await {
                tracing::info!("no path request to the home node: {error}");
            }
        });
    }

    /// Fold an event that changes nothing the screen shows by itself.
    fn quiet(&mut self, event: UiEvent) {
        let _ = update(&mut self.model, event);
    }

    /// Fold one event, keep what it changed, run what it asks for.
    fn apply(&mut self, event: UiEvent) {
        let shown = matches!(
            event,
            UiEvent::Engine(_)
                | UiEvent::SendDone(_)
                | UiEvent::SendUnproven(_)
                | UiEvent::FetchDone(_)
                | UiEvent::Open { .. }
                | UiEvent::Close
                | UiEvent::Write { .. }
                | UiEvent::React { .. }
        );
        let toasts = self.model.toasts.len();
        let effects = update(&mut self.model, event);
        for toast in self.model.toasts.iter().skip(toasts) {
            let _ = self.events.send(Event::Notice {
                v: VERSION,
                text: toast.text.clone(),
            });
        }
        self.dirty |= shown || !effects.is_empty();
        self.persist();
        for effect in effects {
            match effect {
                UiEffect::Send { to, body, fields } => self.sends.push_back((to, body, fields)),
                UiEffect::Fetch => self.pull_owed = true,
                UiEffect::Log(line) | UiEffect::Notice(line) => tracing::info!("{line}"),
                // What the terminal does with its screen, its contacts file
                // and its maps has no place here yet.
                _ => {}
            }
        }
        self.start_next();
    }

    /// The fold's store writes, and its marks to the home node.
    fn persist(&mut self) {
        let writes = self.model.take_writes();
        if let Err(error) = self.store.write(&writes) {
            let _ = self.events.send(Event::Notice {
                v: VERSION,
                text: format!("not saved: {error}"),
            });
        }
        let marks = self.model.take_marks();
        if marks.is_empty() {
            return;
        }
        self.runtime
            .spawn(runner::hand_marks(self.client.clone(), marks, BUDGET));
    }

    /// The oldest send, if none runs; the owed pull, if none runs.
    fn start_next(&mut self) {
        if !self.sending {
            if let Some((to, body, fields)) = self.sends.pop_front() {
                self.sending = true;
                let mut tap = self.fanout.tap();
                let client = self.client.clone();
                let done = self.finished_tx.clone();
                self.runtime.spawn(async move {
                    let event =
                        runner::send_home(&mut tap, &client, to, body, fields, BUDGET, SETTLE)
                            .await;
                    let _ = done.send(Finished::Send(event));
                });
            }
        }
        if !self.pulling && std::mem::take(&mut self.pull_owed) {
            self.pulling = true;
            let _ = self.events.send(Event::Sync {
                v: VERSION,
                state: "running".to_string(),
                detail: String::new(),
            });
            let mut tap = self.fanout.tap();
            let client = self.client.clone();
            let home = self.home.clone();
            let done = self.finished_tx.clone();
            self.runtime.spawn(async move {
                let (result, cursor) = runner::pull(&mut tap, &client, &home, BUDGET).await;
                let _ = done.send(Finished::Pull { result, cursor });
            });
        }
    }

    /// A pull ended: what it brought is in the store by now (the pump
    /// folded it before the runner saw its fence), so the cursor may pass it.
    fn pulled(&mut self, result: Result<usize, String>, cursor: Option<u64>) {
        self.pulling = false;
        runner::settle_pull(&self.home, cursor);
        let (state, detail) = match &result {
            Ok(entries) => ("done", entries.to_string()),
            Err(error) => ("failed", error.clone()),
        };
        let _ = self.events.send(Event::Sync {
            v: VERSION,
            state: state.to_string(),
            detail,
        });
        self.apply(UiEvent::FetchDone(result));
    }

    fn conversations(&self) -> schema::Conversations {
        let rows = self
            .model
            .order()
            .into_iter()
            .map(|index| {
                let conversation = &self.model.conversations[index];
                let last = conversation.messages.last();
                schema::ConversationRow {
                    peer: loquax::address::to_hex(&conversation.peer),
                    name: self.model.names.label(&conversation.peer),
                    last: last.map(preview).unwrap_or_default(),
                    ours: last.is_some_and(|m| m.outbound),
                    at: last.and_then(|m| m.at).map_or(0, |at| at.unix_secs),
                    unread: u32::try_from(conversation.unread).unwrap_or(u32::MAX),
                }
            })
            .collect();
        schema::Conversations {
            v: VERSION,
            conversations: rows,
        }
    }

    fn messages(&self, peer: [u8; 16]) -> schema::Messages {
        let conversation = self.model.conversations.iter().find(|c| c.peer == peer);
        let rows = conversation.map_or_else(Vec::new, |conversation| {
            let chips = reactions::gather(conversation);
            conversation
                .messages
                .iter()
                .filter(|message| !message.is_reaction())
                .map(|message| row(message, &chips))
                .collect()
        });
        schema::Messages {
            v: VERSION,
            peer: loquax::address::to_hex(&peer),
            name: self.model.names.label(&peer),
            messages: rows,
        }
    }
}

/// One bubble as the app draws it.
fn row(
    message: &Message,
    chips: &std::collections::BTreeMap<[u8; 32], Vec<reactions::Chip>>,
) -> schema::MessageRow {
    let chips = message
        .id
        .and_then(|id| chips.get(&id))
        .map(|chips| {
            chips
                .iter()
                .map(|chip| schema::Chip {
                    content: chip.content.clone(),
                    count: u32::try_from(chip.count).unwrap_or(u32::MAX),
                    ours: chip.own,
                })
                .collect()
        })
        .unwrap_or_default();
    schema::MessageRow {
        id: message
            .id
            .map(|id| loquax::address::to_hex(&id))
            .unwrap_or_default(),
        ours: message.outbound,
        body: match message.is_report() {
            true => "position report".to_string(),
            false => message.body.clone(),
        },
        state: match message.outbound {
            true => state_word(message.delivery).to_string(),
            false => String::new(),
        },
        at: message.at.map_or(0, |at| at.unix_secs),
        chips,
    }
}

/// The five states the app draws, from the glyph table's nine.
pub fn state_word(state: Option<DeliveryState>) -> &'static str {
    match state {
        None | Some(DeliveryState::Queued) | Some(DeliveryState::Stamping) => "queued",
        Some(DeliveryState::InFlight) => "sending",
        Some(DeliveryState::Sent) | Some(DeliveryState::InMailbox) => "sent",
        Some(DeliveryState::Delivered) => "delivered",
        Some(DeliveryState::Rejected) | Some(DeliveryState::Failed) => "failed",
    }
}

/// The list's line for a conversation's last message.
fn preview(message: &Message) -> String {
    if let Some(text) = reactions::preview(message) {
        return text;
    }
    if message.is_report() {
        return "position report".to_string();
    }
    message.body.lines().next().unwrap_or_default().to_string()
}

/// A join that failed, in words for the join screen.
fn join_words(error: &loquax::home::client::HomeError, server: &str) -> String {
    use loquax::home::client::HomeError;
    match error {
        HomeError::NoPath => format!(
            "The home node did not answer through {server}. Check the address, \
             that its lnsd runs a TCP server there, and that this phone can reach it."
        ),
        HomeError::Refused(status) => format!("The home node refused the code: {status}."),
        HomeError::Timeout => "The home node did not answer in time. Try again.".to_string(),
        other => format!("Joining failed: {other}."),
    }
}

/// Whether the leaf's interface to the home node is up: its only one, the
/// TCP client, which is down from the moment the server goes until it
/// redials.
fn link_up(node: &leviculum_std::driver::ReticulumNode) -> bool {
    node.interface_stats()
        .iter()
        .any(|interface| !interface.is_local_client && interface.online)
}

fn wall_now(offset: i32) -> WallTime {
    WallTime {
        unix_secs: leviculum_std::local_time::unix_now_secs(),
        offset_secs: offset,
    }
}

fn load_server(home: &Path) -> Option<String> {
    std::fs::read_to_string(home.join(SERVER_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn save_server(home: &Path, server: &str) -> Result<(), String> {
    loquax::home::client::write_atomically(&home.join(SERVER_FILE), &format!("{server}\n"))
}

/// A peer's address as the app hands it back.
pub fn parse_peer(text: &str) -> Result<[u8; 16], String> {
    loquax::address::parse(text.trim())
        .map_err(|error| format!("'{}' is not an address: {error}", text.trim()))
}

/// A message id in hex.
pub fn parse_id(text: &str) -> Result<[u8; 32], String> {
    let text = text.trim();
    loquax::address::parse_id(text).ok_or_else(|| format!("'{text}' is not a message id"))
}
