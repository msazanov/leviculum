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
//! `unsafe_code` is denied and allowed once per exported symbol: an
//! exported, unmangled name is what the lint counts, and it is what JNI
//! looks a native method up by. loquax itself keeps `forbid`.

#![deny(unsafe_code)]

use std::path::Path;

use jni::objects::{JClass, JString};
use jni::EnvUnowned;

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
