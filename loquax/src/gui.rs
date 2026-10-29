//! `loquax gui`: the window over the same shell and model as the terminal
//! (`docs/src/concepts/gui-toolkit.md`, option B, iced on the desktop;
//! `docs/src/concepts/loquax-map.md`, slice (d)). Its first view is the
//! map: the peers with a position listed on the left, by the display rule
//! with their fix's age and dimmed when stale, and the map on the right.
//!
//! Two halves on two threads:
//!
//! - The shell ([`crate::shell::Shell`]) runs on the tokio runtime, as in
//!   the terminal: engine, store, sends and fetches, the mailbox's schedule,
//!   the model and its clock. [`drive`] pumps it and ticks it, and after
//!   every change sends what the window shows ([`Seen`]).
//! - The window runs iced on the main thread. It holds only what is the
//!   window's: the map's centre, zoom and selected pin (the model's
//!   [`MapState`], driven by the model's own map functions in screen
//!   pixels, [`map`]), the picture on screen, and the painter, whose paints
//!   run on iced's executor so a drag never waits for one.
//!
//! The map's autoload (1089) runs in the shell: the window decides what to
//! ask with the model's own rules ([`want_pins`] when the pins first arrive,
//! [`want_view`] on `d`) and sends it over; the shell's [`Seen`] carries the
//! loading word and the maps directory's epoch, and a new epoch reads the
//! regions again and paints the view anew.

pub mod map;

use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::widget::{button, column, container, image, row, scrollable, stack, text};
use iced::{
    keyboard, mouse, window, Color, Element, Length, Point, Rectangle, Renderer, Size,
    Subscription, Task, Theme,
};
use tokio::sync::oneshot;

use leviculum_mapfill::style::Style;
use loquax::picture::{map_theme, Painter};
use loquax::ui::map::{
    fit, viewport, want_pins, want_view, zoom_at, MapPin, MapPress, MapSetup, MapState, MapWant,
};
use loquax::ui::{UiEvent, MAP_LOADED_ALREADY};

use crate::shell::{self, local_now, Shell, ShellOptions, PUMP_MS, TICK_MS};

/// The list of peers on the left, in logical pixels.
const LIST_WIDTH: f32 = 260.0;
/// The window's size when it opens.
const WINDOW: (f32, f32) = (1100.0, 720.0);

/// What the window shows of the model, sent after every change.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// The map's pins in `Tab` order: every peer with a position, our own
    /// named `you`, each by the display rule with its fix's age.
    pub pins: Vec<MapPin>,
    /// The newest note the model holds, said in the bar.
    pub notice: Option<String>,
    /// The word while a map region loads.
    pub loading: Option<String>,
    /// The maps directory's epoch: a new one means a region landed or left.
    pub maps: u64,
    /// Whether going to a place loads its region (`map_autoload`).
    pub autoload: bool,
    /// How the map is drawn: the PC's finish in the table `map_theme`
    /// names, light for `auto`, the window having no theme of its own.
    pub style: Style,
}

impl Seen {
    fn of(shell: &Shell) -> Self {
        Self {
            pins: shell.model.map_pins(),
            notice: shell.model.toasts.last().map(|t| t.text.clone()),
            loading: shell.model.map_loading.clone(),
            maps: shell.maps.epoch(),
            autoload: shell.model.map_autoload,
            style: Style::PC.in_theme(map_theme(shell.model.map_theme, None)),
        }
    }
}

/// `loquax gui`: the shell started as the terminal starts it, driven on
/// the runtime while the window runs, and finished as the terminal
/// finishes it.
pub async fn run(options: ShellOptions) -> ExitCode {
    let (shell, _config) = match shell::start(&options).await {
        Ok(started) => started,
        Err(code) => return code,
    };
    let painter = Painter::new(shell.maps.clone());
    let (seen_tx, seen_rx) = unbounded();
    let (asks_tx, asks_rx) = tokio::sync::mpsc::unbounded_channel();
    let (leave_tx, leave_rx) = oneshot::channel();
    let driver = tokio::spawn(drive(shell, leave_rx, seen_tx, asks_rx));
    // iced runs its own loop on this thread until the window closes; the
    // runtime's other threads drive the shell meanwhile.
    let shown = tokio::task::block_in_place(|| window(painter, seen_rx, asks_tx));
    let _ = leave_tx.send(());
    let shell = match driver.await {
        Ok(shell) => shell,
        Err(error) => return crate::failure(error, "failed"),
    };
    shell::finish(shell, shown.map_err(|e| e.to_string())).await
}

/// The shell's loop while the window is open: the engine's queue every
/// [`PUMP_MS`], the clock every [`TICK_MS`], and what the window shows sent
/// whenever it changed. Ends when the window leaves or the model quits.
async fn drive(
    mut shell: Shell,
    mut leave: oneshot::Receiver<()>,
    seen: UnboundedSender<Seen>,
    mut asks: tokio::sync::mpsc::UnboundedReceiver<MapWant>,
) -> Shell {
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pump = tokio::time::interval(Duration::from_millis(PUMP_MS));
    pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    shell.apply(UiEvent::Clock(local_now()));
    let mut clocks = (local_now().unix_secs, shell.elapsed_ms());
    let mut last: Option<Seen> = None;
    loop {
        // The map view asks for no copy, link or mouse capture; what the
        // model asks of a terminal has no meaning in the window yet.
        shell.outputs.clear();
        let now = Seen::of(&shell);
        if last.as_ref() != Some(&now) {
            if seen.unbounded_send(now.clone()).is_err() {
                break;
            }
            last = Some(now);
        }
        if shell.model.quit {
            break;
        }
        tokio::select! {
            _ = &mut leave => break,
            Some(want) = asks.recv() => shell.map_ask(want),
            _ = pump.tick() => shell.poll(),
            _ = tick.tick() => shell.tick(&mut clocks),
        }
    }
    shell
}

/// Open the window and run it until it is closed.
fn window(painter: Painter, seen: UnboundedReceiver<Seen>, asks: Asks) -> iced::Result {
    // iced asks its boot function for the first state; it is asked once,
    // and what it needs is handed over through this slot.
    let slot = Mutex::new(Some((painter, seen, asks)));
    iced::application(
        move || {
            let taken = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            App::boot(taken)
        },
        App::update,
        App::view,
    )
    .title("loquax")
    .subscription(App::subscription)
    .window_size(Size::new(WINDOW.0, WINDOW.1))
    .run()
}

/// A pointer on the map, in logical pixels from its top left corner.
#[derive(Clone, Copy, Debug)]
pub enum Pointer {
    Down(Point),
    Move(Point),
    Up,
    /// The wheel turned at a point: towards the reader zooms in.
    Wheel(Point, bool),
}

#[derive(Clone, Debug)]
pub enum Message {
    Seen(Seen),
    /// The map's canvas has this logical size.
    MapSize(Size),
    Scale(f32),
    Window(window::Event),
    Key(keyboard::Event),
    Pointer(Pointer),
    /// A peer clicked in the list.
    Select([u8; 16]),
    Painted(Arc<map::Picture>),
}

/// Where the window sends the regions it asks the shell to load.
type Asks = tokio::sync::mpsc::UnboundedSender<MapWant>;

/// The window's state.
struct App {
    seen: Seen,
    asks: Option<Asks>,
    /// The view the last fit chose: a region landing under it fits again.
    fitted_at: Option<(leviculum_map::mercator::World, u8)>,
    painter: Arc<Mutex<Painter>>,
    setup: MapSetup,
    state: MapState,
    /// The map's canvas in logical pixels, once laid out.
    size: Option<Size>,
    /// Screen pixels per logical pixel.
    scale: f32,
    /// The view fitted to the pins once they first arrived.
    fitted: bool,
    press: Option<MapPress>,
    /// The picture on screen, and its image.
    picture: Option<(Arc<map::Picture>, iced::advanced::image::Handle)>,
    /// The pins over the picture on screen.
    placed: Vec<map::Placed>,
    /// A paint is running; a view asked for meanwhile is painted after it.
    painting: bool,
}

impl App {
    fn boot(taken: Option<(Painter, UnboundedReceiver<Seen>, Asks)>) -> (Self, Task<Message>) {
        let (painter, seen, asks) = match taken {
            Some((painter, seen, asks)) => (painter, Some(seen), Some(asks)),
            None => (Painter::new(loquax::picture::Maps::default()), None, None),
        };
        let setup = map::setup(&painter);
        let state = MapState {
            centre: fit(&[], map::area((1, 1)), &setup).0,
            zoom: setup.zooms.0,
            selected: None,
        };
        let app = Self {
            seen: Seen {
                pins: Vec::new(),
                notice: None,
                loading: None,
                maps: 0,
                autoload: false,
                style: Style::PC,
            },
            asks,
            fitted_at: None,
            painter: Arc::new(Mutex::new(painter)),
            setup,
            state,
            size: None,
            scale: 1.0,
            fitted: false,
            press: None,
            picture: None,
            placed: Vec::new(),
            painting: false,
        };
        let scale = window::oldest()
            .and_then(window::scale_factor)
            .map(Message::Scale);
        let seen = match seen {
            Some(seen) => Task::run(seen, Message::Seen),
            None => Task::none(),
        };
        (app, Task::batch([scale, seen]))
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::events().map(|(_, event)| Message::Window(event)),
            keyboard::listen().map(Message::Key),
        ])
    }

    /// The map's size in screen pixels.
    fn pixels(&self) -> Option<(u32, u32)> {
        let size = self.size?;
        let px = |v: f32| (v * self.scale).round().max(1.0) as u32;
        Some((px(size.width), px(size.height)))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Seen(seen) => {
                let landed = seen.maps != self.seen.maps;
                self.seen = seen;
                if landed {
                    self.regions_changed();
                }
                if !self.fitted {
                    self.fit();
                }
                self.place();
            }
            Message::MapSize(size) => {
                self.size = Some(size);
                if !self.fitted {
                    self.fit();
                }
            }
            Message::Scale(scale) => self.scale = scale.max(0.25),
            Message::Window(window::Event::Rescaled(scale)) => self.scale = scale.max(0.25),
            Message::Window(_) => return Task::none(),
            Message::Key(event) => self.key(event),
            Message::Pointer(pointer) => self.pointer(pointer),
            Message::Select(peer) => {
                self.state.selected = Some(peer);
                if let Some(pin) = self.seen.pins.iter().find(|p| p.peer == peer) {
                    self.state.centre = pin.at;
                }
                self.place();
            }
            Message::Painted(picture) => {
                self.painting = false;
                let (w, h) = picture.view.size();
                let handle = iced::advanced::image::Handle::from_rgba(w, h, picture.rgba.clone());
                self.picture = Some((picture, handle));
                self.place();
            }
        }
        self.paint()
    }

    /// Every pin in view, as `0` does; before the first pins and the
    /// canvas, nothing. The first fit with pins asks for the places no
    /// region holds (1089).
    fn fit(&mut self) {
        let Some(size) = self.pixels() else {
            return;
        };
        let (centre, zoom) = fit(&self.seen.pins, map::area(size), &self.setup);
        self.state.centre = centre;
        self.state.zoom = zoom;
        self.fitted_at = Some((centre, zoom));
        if self.state.selected.is_none() {
            self.state.selected = self.seen.pins.first().map(|p| p.peer);
        }
        let first = !self.fitted && !self.seen.pins.is_empty();
        self.fitted = !self.seen.pins.is_empty();
        if first && self.seen.autoload {
            for want in want_pins(&self.seen.pins, &self.setup.regions) {
                self.ask(want);
            }
        }
    }

    fn ask(&self, want: MapWant) {
        if let Some(asks) = &self.asks {
            let _ = asks.send(want);
        }
    }

    /// A region landed or left: the regions read again, the setup with
    /// them, and the fit again when the reader has not moved since it.
    fn regions_changed(&mut self) {
        {
            let mut painter = self.painter.lock().unwrap_or_else(PoisonError::into_inner);
            painter.refresh();
            self.setup = map::setup(&painter);
        }
        if self.fitted_at == Some((self.state.centre, self.state.zoom)) {
            let selected = self.state.selected;
            self.fit();
            self.state.selected = selected.or(self.state.selected);
        }
    }

    /// The pins over the picture on screen: placed for its view, so they
    /// never stand off the streets while a newer view is painted.
    fn place(&mut self) {
        let Some((picture, _)) = &self.picture else {
            return;
        };
        self.placed = map::place(
            &self.seen.pins,
            &picture.view,
            self.state.selected,
            self.scale,
            map::measure,
        );
    }

    /// Ask for the view the state shows, unless it is on screen or a
    /// paint is running; the paint's end asks again.
    fn paint(&mut self) -> Task<Message> {
        let Some(size) = self.pixels() else {
            return Task::none();
        };
        let view = viewport(&self.state, map::area(size), &self.setup);
        let current = self.picture.as_ref().is_some_and(|(p, _)| {
            p.view == view && p.epoch == self.seen.maps && p.style == self.seen.style
        });
        if current || self.painting {
            return Task::none();
        }
        self.painting = true;
        let (painter, scale, style) = (self.painter.clone(), self.scale, self.seen.style);
        Task::perform(
            async move {
                let mut painter = painter.lock().unwrap_or_else(PoisonError::into_inner);
                Arc::new(map::paint(&mut painter, &view, style, scale))
            },
            Message::Painted,
        )
    }

    fn key(&mut self, event: keyboard::Event) {
        let keyboard::Event::KeyPressed { modified_key, .. } = event else {
            return;
        };
        let keyboard::Key::Character(c) = modified_key.as_ref() else {
            return;
        };
        let zooms = self.setup.zooms;
        match c {
            "+" | "=" => self.state.zoom = self.state.zoom.saturating_add(1).min(zooms.1),
            "-" => self.state.zoom = self.state.zoom.saturating_sub(1).max(zooms.0),
            "0" => {
                let selected = self.state.selected;
                self.fit();
                self.state.selected = selected.or(self.state.selected);
            }
            // The visible area's region, as `d` on the terminal's map.
            "d" => {
                let Some(size) = self.pixels() else {
                    return;
                };
                let view = viewport(&self.state, map::area(size), &self.setup);
                match want_view(&view, &self.setup.regions) {
                    Some(want) => self.ask(want),
                    None => self.seen.notice = Some(MAP_LOADED_ALREADY.to_string()),
                }
            }
            _ => {}
        }
    }

    fn pointer(&mut self, pointer: Pointer) {
        let Some(size) = self.pixels() else {
            return;
        };
        let at = |p: Point| {
            let clamp = |v: f32| (v * self.scale).round().clamp(0.0, f32::from(u16::MAX)) as u16;
            (clamp(p.x), clamp(p.y))
        };
        match pointer {
            Pointer::Down(p) => {
                let (x, y) = at(p);
                let pin = map::hit(&self.placed, f32::from(x), f32::from(y), self.scale);
                self.press = Some(MapPress::new((x, y), self.state.centre, pin));
            }
            Pointer::Move(p) => {
                let (x, y) = at(p);
                let Some(press) = self.press.as_mut() else {
                    return;
                };
                let far = (map::DRAG * self.scale) as u16;
                press.dragging |=
                    x.abs_diff(press.from.0) >= far || y.abs_diff(press.from.1) >= far;
                self.state.centre = press.centre_at(x, y, &self.state, &self.setup);
            }
            Pointer::Up => {
                if let Some(press) = self.press.take() {
                    if !press.dragging {
                        if let Some(peer) = press.pin {
                            self.state.selected = Some(peer);
                            self.place();
                        }
                    }
                }
            }
            Pointer::Wheel(p, closer) => {
                let (x, y) = at(p);
                let zooms = self.setup.zooms;
                let zoom = match closer {
                    true => self.state.zoom.saturating_add(1).min(zooms.1),
                    false => self.state.zoom.saturating_sub(1).max(zooms.0),
                };
                zoom_at(&mut self.state, map::area(size), &self.setup, x, y, zoom);
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let list = self
            .seen
            .pins
            .iter()
            .fold(column![].spacing(2), |list, pin| {
                let alpha = if pin.stale { 0.5 } else { 1.0 };
                let age = pin.age.clone().unwrap_or_default();
                let entry = column![text(pin.label.clone()).size(15), text(age).size(12),];
                let style = match Some(pin.peer) == self.state.selected {
                    true => button::primary,
                    false => button::text,
                };
                let button = button(container(entry).width(Length::Fill))
                    .on_press(Message::Select(pin.peer))
                    .style(move |theme: &Theme, status| {
                        let mut s = style(theme, status);
                        s.text_color = s.text_color.scale_alpha(alpha);
                        s
                    })
                    .width(Length::Fill);
                list.push(button)
            });
        let list: Element<'_, Message> = match self.seen.pins.is_empty() {
            true => text("no positions yet").size(14).into(),
            false => scrollable(list).into(),
        };
        let credit = self
            .picture
            .as_ref()
            .map_or(String::new(), |(p, _)| p.attribution.clone());
        let bar = row![
            // Always on screen: the region's credit, or the outline's.
            text(credit).size(13),
            container(
                text(
                    [self.seen.loading.clone(), self.seen.notice.clone()]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" · ")
                )
                .size(13)
            )
            .width(Length::Fill)
            .align_right(Length::Fill),
            text(format!("  z{}", self.state.zoom)).size(13),
        ]
        .padding([2, 8]);
        // The picture is a widget of its own under the canvas: iced draws a
        // canvas's images above its fills, which would hide the markers.
        let picture: Element<'_, Message> = match &self.picture {
            Some((p, handle)) => {
                let (w, h) = p.view.size();
                image(handle.clone())
                    .width(w as f32 / self.scale)
                    .height(h as f32 / self.scale)
                    .filter_method(iced::advanced::image::FilterMethod::Nearest)
                    .into()
            }
            None => text("").into(),
        };
        // The stack takes its size from its first layer, so the picture
        // sits in a layer that fills the map's place.
        let picture = container(picture).width(Length::Fill).height(Length::Fill);
        let canvas = Canvas::new(MapCanvas { app: self })
            .width(Length::Fill)
            .height(Length::Fill);
        let map = container(
            stack![picture, canvas]
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true);
        row![
            container(list).width(LIST_WIDTH).padding(6),
            column![map, bar],
        ]
        .into()
    }
}

/// The map's canvas over the picture: the pins, their labels, and the
/// pointer.
struct MapCanvas<'a> {
    app: &'a App,
}

/// What the canvas remembers between events.
#[derive(Default)]
struct CanvasState {
    size: Option<Size>,
    pressed: bool,
}

impl canvas::Program<Message> for MapCanvas<'_> {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut CanvasState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if state.size != Some(bounds.size()) {
            state.size = Some(bounds.size());
            return Some(canvas::Action::publish(Message::MapSize(bounds.size())));
        }
        let iced::Event::Mouse(event) = event else {
            return None;
        };
        let inside = cursor.position_in(bounds);
        let pointer = match (event, inside) {
            (mouse::Event::ButtonPressed(mouse::Button::Left), Some(p)) => {
                state.pressed = true;
                Pointer::Down(p)
            }
            (mouse::Event::CursorMoved { .. }, _) if state.pressed => {
                let p = cursor.position()?;
                Pointer::Move(Point::new(p.x - bounds.x, p.y - bounds.y))
            }
            (mouse::Event::ButtonReleased(mouse::Button::Left), _) if state.pressed => {
                state.pressed = false;
                Pointer::Up
            }
            (mouse::Event::WheelScrolled { delta }, Some(p)) => {
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => {
                        *y
                    }
                };
                if y == 0.0 {
                    return None;
                }
                Pointer::Wheel(p, y > 0.0)
            }
            _ => return None,
        };
        Some(canvas::Action::publish(Message::Pointer(pointer)).and_capture())
    }

    fn draw(
        &self,
        _state: &CanvasState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let app = self.app;
        let scale = app.scale;
        for pin in &app.placed {
            let ink = match (pin.selected, pin.stale) {
                (true, _) => Color::from_rgb8(0xd0, 0x30, 0x30),
                (false, false) => Color::from_rgb8(0x20, 0x50, 0xc0),
                (false, true) => Color::from_rgba8(0x20, 0x50, 0xc0, 0.45),
            };
            let centre = Point::new(pin.x / scale, pin.y / scale);
            let radius = map::MARKER / 2.0;
            frame.fill(&Path::circle(centre, radius + 1.5), Color::WHITE);
            frame.fill(&Path::circle(centre, radius), ink);
            if let Some((x, y, words)) = &pin.label {
                let text_ink = match pin.stale {
                    true => Color::from_rgba8(0x20, 0x20, 0x20, 0.55),
                    false => Color::from_rgb8(0x10, 0x10, 0x10),
                };
                let at = Point::new(x / scale, y / scale);
                let width = pin.width / scale;
                let line = map::LABEL * map::LINE;
                // A light ground under the text, so a name reads on any
                // street.
                frame.fill_rectangle(
                    Point::new(at.x - 2.0, at.y),
                    Size::new(width + 4.0, line),
                    Color::from_rgba8(0xff, 0xff, 0xff, 0.75),
                );
                frame.fill_text(canvas::Text {
                    content: words.clone(),
                    position: at,
                    color: text_ink,
                    size: iced::Pixels(map::LABEL),
                    ..canvas::Text::default()
                });
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &CanvasState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match (state.pressed, cursor.is_over(bounds)) {
            (true, _) => mouse::Interaction::Grabbing,
            (false, true) => mouse::Interaction::Grab,
            (false, false) => mouse::Interaction::default(),
        }
    }
}
