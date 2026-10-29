//! loquax on Android: the library the app loads with
//! `System.loadLibrary("loquax_android")`, and the native methods of its
//! Java class `network.leviculum.loquax.Native`.
//!
//! The boundary sits on loquax's shell, not on its screen model: Java draws
//! with Android's own views and asks this library for what to show
//! (`docs/drafts/loquax-android-2026-10.md`, option A). Everything here is
//! plain Rust that the host's tests run; the exported functions only move
//! strings across JNI.
//!
//! The facade (`docs/drafts/loquax-android-2026-10.md`, option A) is five
//! calls on the shell's Outbox seam, each bytes in and bytes out in the
//! msgpack schema of [`schema`]: [`start`], [`command`], [`query`],
//! [`next_event`] (blocking, on a Java thread of its own) and [`stop`].
//! One [`session::Session`] per process, which is what an app is. Beside
//! them the map screen's [`render_map`] (1128), which needs no session:
//! Rust draws the map, Java hosts the bitmap ([`map`]).
//!
//! `unsafe_code` is denied and allowed once per exported symbol: an
//! exported, unmangled name is what the lint counts, and it is what JNI
//! looks a native method up by. loquax itself keeps `forbid`.

#![deny(unsafe_code)]

pub mod map;
pub mod schema;
pub mod session;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::jint;
use jni::EnvUnowned;

use schema::{Done, Event, Started, VERSION};
use session::Session;

/// The process's session, once started.
static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);

fn current() -> Option<Arc<Session>> {
    SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Start the session from a [`schema::Start`]; a [`schema::Started`] back.
/// A session already running answers for itself: the Activity starts again
/// on every rotation, the session does not. One that ended by itself (its
/// engine died) is stopped and replaced, never handed out again.
pub fn start(request: &[u8]) -> Vec<u8> {
    let started = match schema::decode::<schema::Start>(request) {
        Err(error) => Started {
            v: VERSION,
            error: Some(error),
            ..Started::default()
        },
        Ok(start) => {
            let mut slot = SESSION
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(dead) = slot.take_if(|session| session.is_over()) {
                dead.stop();
            }
            let running = match slot.as_ref() {
                Some(session) => Ok(Arc::clone(session)),
                None => Session::start(&start).map(Arc::new),
            };
            match running {
                Ok(session) => {
                    *slot = Some(Arc::clone(&session));
                    Started {
                        v: VERSION,
                        ok: true,
                        error: None,
                        need: None,
                        address: Some(loquax::address::to_hex(&session.address())),
                        server: Some(session.server().to_string()),
                        link: session.link(),
                    }
                }
                Err(error) => Started {
                    v: VERSION,
                    ok: false,
                    error: Some(error.text),
                    need: error.need.map(str::to_string),
                    address: None,
                    server: None,
                    link: false,
                },
            }
        }
    };
    schema::encode(&started)
}

/// One [`schema::Request`]; a [`schema::Done`] back.
pub fn command(request: &[u8]) -> Vec<u8> {
    let done = match (current(), schema::decode::<schema::Request>(request)) {
        (None, _) => Done::failed("not started"),
        (_, Err(error)) => Done::failed(error),
        (Some(session), Ok(request)) => match session.request(request) {
            Ok(()) => Done::ok(),
            Err(error) => Done::failed(error),
        },
    };
    schema::encode(&done)
}

/// One [`schema::Query`]; its answer, or a [`schema::Done`] with the error.
pub fn query(request: &[u8]) -> Vec<u8> {
    let answer = match (current(), schema::decode::<schema::Query>(request)) {
        (None, _) => Err("not started".to_string()),
        (_, Err(error)) => Err(error),
        (Some(session), Ok(query)) => session.query(query),
    };
    answer.unwrap_or_else(|error| schema::encode(&Done::failed(error)))
}

/// The next [`schema::Event`], waiting at most `wait`; `None` when none
/// came. Without a session, `stopped` at once. The `stopped` of a session
/// that ended by itself also clears the slot, so every call after it says
/// "not started" and the service's next `start` builds a fresh session.
pub fn next_event(wait: Duration) -> Option<Vec<u8>> {
    let event = match current() {
        Some(session) => {
            let event = session.next_event(wait)?;
            if matches!(event, Event::Stopped { .. }) && session.is_over() {
                forget(&session);
            }
            event
        }
        None => Event::Stopped {
            v: VERSION,
            detail: "not started".to_string(),
        },
    };
    Some(schema::encode(&event))
}

/// One frame of the map screen for a [`schema::MapRequest`], laid out as
/// [`schema::frame`] says. The pins are the running session's, or the
/// store's in the request's directory without one.
pub fn render_map(request: &[u8]) -> Vec<u8> {
    match schema::decode::<schema::MapRequest>(request) {
        Err(error) => schema::frame(&map::failed(error), &[]),
        Ok(request) => {
            let pins = current().map(|session| session.map_pins());
            let now = loquax::when::WallTime {
                unix_secs: leviculum_std::local_time::unix_now_secs(),
                offset_secs: 0,
            };
            map::render(&request, pins, now)
        }
    }
}

/// Clear the slot if it still holds `session`, and stop it: its runtime
/// and node go, its files stay for the next start.
fn forget(session: &Arc<Session>) {
    let gone = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take_if(|held| Arc::ptr_eq(held, session));
    if let Some(gone) = gone {
        gone.stop();
    }
}

/// Cut the running session off its engine, as the engine's death does;
/// `false` without one. For the facade test, which has no other way to
/// kill an engine.
#[doc(hidden)]
pub fn simulate_engine_death() -> bool {
    match current() {
        Some(session) => {
            session.simulate_engine_death();
            true
        }
        None => false,
    }
}

/// Stop the session, if one runs.
pub fn stop() {
    let session = SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(session) = session {
        session.stop();
    }
}

/// What the app's first screen says about this device: the loquax version,
/// and whether `home` (the app's files directory, which is its loquax home)
/// makes it a client of a home node, with the home node's LXMF address and
/// the last archive entry pulled.
pub fn describe(home: &Path) -> String {
    let state = match loquax::home::client::load_client(home) {
        Ok(None) => "Not a client of a home node.".to_string(),
        Ok(Some(key)) => match loquax::home::lxmf_address(&key) {
            Ok(address) => format!(
                "Client of the home node {}, last entry pulled {}.",
                loquax::address::to_hex(&address),
                loquax::home::client::load_cursor(home)
            ),
            Err(error) => error,
        },
        Err(error) => error,
    };
    format!("loquax {}\n{state}", loquax::VERSION)
}

/// `static native String describe(String home)` of `Native`: [`describe`].
/// A panic or a JNI failure becomes a Java `RuntimeException`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_describe<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    home: JString<'caller>,
) -> JString<'caller> {
    unowned
        .with_env(|env| -> Result<_, jni::errors::Error> {
            let home = home.to_string();
            JString::from_str(env, describe(Path::new(&home)))
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// The bytes of a Java `byte[]`, empty for `null`.
fn bytes_of(env: &mut jni::Env<'_>, array: &JByteArray<'_>) -> Result<Vec<u8>, jni::errors::Error> {
    match array.is_null() {
        true => Ok(Vec::new()),
        false => env.convert_byte_array(array),
    }
}

/// `static native byte[] start(byte[] request)` of `Native`: [`start`].
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_start<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    request: JByteArray<'caller>,
) -> JByteArray<'caller> {
    unowned
        .with_env(|env| -> Result<_, jni::errors::Error> {
            let request = bytes_of(env, &request)?;
            env.byte_array_from_slice(&start(&request))
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// `static native byte[] command(byte[] request)` of `Native`: [`command`].
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_command<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    request: JByteArray<'caller>,
) -> JByteArray<'caller> {
    unowned
        .with_env(|env| -> Result<_, jni::errors::Error> {
            let request = bytes_of(env, &request)?;
            env.byte_array_from_slice(&command(&request))
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// `static native byte[] query(byte[] request)` of `Native`: [`query`].
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_query<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    request: JByteArray<'caller>,
) -> JByteArray<'caller> {
    unowned
        .with_env(|env| -> Result<_, jni::errors::Error> {
            let request = bytes_of(env, &request)?;
            env.byte_array_from_slice(&query(&request))
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// `static native byte[] renderMap(byte[] request)` of `Native`:
/// [`render_map`]. Called off the UI thread.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_renderMap<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    request: JByteArray<'caller>,
) -> JByteArray<'caller> {
    unowned
        .with_env(|env| -> Result<_, jni::errors::Error> {
            let request = bytes_of(env, &request)?;
            env.byte_array_from_slice(&render_map(&request))
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// `static native byte[] nextEvent(int waitMs)` of `Native`:
/// [`next_event`], `null` when none came.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_nextEvent<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    wait_ms: jint,
) -> JByteArray<'caller> {
    unowned
        .with_env(|env| -> Result<_, jni::errors::Error> {
            let wait = Duration::from_millis(u64::try_from(wait_ms).unwrap_or(0));
            match next_event(wait) {
                Some(event) => env.byte_array_from_slice(&event),
                None => Ok(JByteArray::default()),
            }
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

/// `static native void stop()` of `Native`: [`stop`].
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "system" fn Java_network_leviculum_loquax_Native_stop<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) {
    unowned
        .with_env(|_env| -> Result<_, jni::errors::Error> {
            stop();
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::Identity;

    #[test]
    fn a_fresh_home_is_no_client() {
        let home = tempfile::tempdir().expect("tempdir");
        let text = describe(home.path());
        assert!(text.starts_with(&format!("loquax {}\n", loquax::VERSION)));
        assert!(text.ends_with("Not a client of a home node."), "{text}");
    }

    #[test]
    fn a_client_names_its_home_node_and_cursor() {
        let home = tempfile::tempdir().expect("tempdir");
        let identity = Identity::generate(&mut rand_core::OsRng);
        let key: [u8; 64] = identity.public_key_bytes();
        loquax::home::client::save_client(home.path(), &key).expect("client");
        loquax::home::client::save_cursor(home.path(), 42).expect("cursor");
        let address = loquax::home::lxmf_address(&key).expect("address");
        let text = describe(home.path());
        assert!(
            text.ends_with(&format!(
                "Client of the home node {}, last entry pulled 42.",
                loquax::address::to_hex(&address)
            )),
            "{text}"
        );
    }

    #[test]
    fn a_damaged_client_file_is_named() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join(loquax::home::client::CLIENT_FILE),
            "not hex\n",
        )
        .expect("write");
        let text = describe(home.path());
        assert!(text.contains(loquax::home::client::CLIENT_FILE), "{text}");
        assert!(text.contains("not a home identity's public key"), "{text}");
    }
}
