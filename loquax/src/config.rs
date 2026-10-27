//! `loquax`'s own tiny config file.
//!
//! `${LNMSG_HOME}/config` (the same directory as the identity — see
//! [`crate::identity::home_dir`]) holds `key = value` lines. Four keys:
//! `propagation_node`, the persisted default for `--pn`; `send_on_enter`,
//! whether `Enter` in the compose pane sends (`true`, the default) or breaks
//! the line (`false`, and the modifier chord sends); `theme`, `auto`
//! (the default: ask the terminal), `dark` or `light`, which `--theme`
//! overrides; and `sync_interval`, the longest the messenger leaves the
//! mailbox unsynced (`15m` by default, `off` for start and the key only),
//! which `--sync-interval` overrides:
//!
//! ```text
//! # the mailbox this machine uses when a direct delivery fails
//! propagation_node = 76cff79ab2f43bebcb7398e08e56ee0f
//! # Enter breaks the line; Alt-Enter (Ctrl-Enter where the terminal can) sends
//! send_on_enter = false
//! # a light terminal that does not answer the background query
//! theme = light
//! # sync the mailbox at least every half hour, not every fifteen minutes
//! sync_interval = 30m
//! ```
//!
//! Deliberately not the Reticulum config file: that one belongs to the
//! daemon and is shared with Python's `rnsd`, and a foreign key in it would
//! be a compatibility hazard. And deliberately hand-parsed rather than a
//! TOML dependency: three keys a decade from now do not justify a parser
//! crate in a messenger's supply chain.
//!
//! A missing file means defaults. A present-but-wrong value is an error
//! that names the file and line, never silently ignored: a typo in a
//! mailbox hash would otherwise turn into "my mail goes nowhere" with no
//! trace.

use alloc::string::{String, ToString};

use leviculum_ui::theme::ThemeFlag;

use crate::address;

/// The config file's name under the loquax home directory.
pub const CONFIG_FILE: &str = "config";

/// Why the config text could not be used. It names the line; the file it came
/// from is the shell's to add ([`LoadError`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// A line that should carry a value does not parse. 1-based line number.
    BadValue {
        line: usize,
        key: &'static str,
        detail: String,
    },
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadValue { line, key, detail } => write!(f, "line {line}: {key}: {detail}"),
        }
    }
}

impl core::error::Error for ConfigError {}

/// What the config file contributes to a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The persisted default propagation node, `--pn`'s fallback.
    pub propagation_node: Option<[u8; 16]>,
    /// `send_on_enter`, when the file sets it; unset is on.
    pub send_on_enter: Option<bool>,
    /// `theme`, when the file sets it; unset is `auto`.
    pub theme: Option<ThemeFlag>,
    /// `sync_interval`, when the file sets it: the mailbox schedule's bound
    /// in milliseconds, `None` inside for the timer off
    /// ([`crate::sync::parse_interval`]); unset is fifteen minutes.
    pub sync_interval: Option<Option<u64>>,
}

/// Read the config file's text.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    let mut config = Config::default();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Unknown keys pass silently: a config written by a newer loquax
        // must not stop an older one from sending at all.
        if key.trim() == "send_on_enter" {
            config.send_on_enter = Some(match value.trim().trim_matches('"') {
                "true" | "yes" | "on" => true,
                "false" | "no" | "off" => false,
                other => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "send_on_enter",
                        detail: alloc::format!("{other:?} is not true or false"),
                    });
                }
            });
        }
        if key.trim() == "theme" {
            config.theme = Some(match value.trim().trim_matches('"') {
                "auto" => ThemeFlag::Auto,
                "dark" => ThemeFlag::Dark,
                "light" => ThemeFlag::Light,
                other => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "theme",
                        detail: alloc::format!("{other:?} is not auto, dark or light"),
                    });
                }
            });
        }
        if key.trim() == "sync_interval" {
            match crate::sync::parse_interval(value.trim().trim_matches('"')) {
                Ok(bound) => config.sync_interval = Some(bound),
                Err(detail) => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "sync_interval",
                        detail,
                    });
                }
            }
        }
        if key.trim() == "propagation_node" {
            let value = value.trim().trim_matches('"');
            match address::parse(value) {
                Ok(hash) => config.propagation_node = Some(hash),
                Err(error) => {
                    return Err(ConfigError::BadValue {
                        line: index + 1,
                        key: "propagation_node",
                        detail: error.to_string(),
                    });
                }
            }
        }
    }
    Ok(config)
}

/// Why `home/config` could not be used, with the file named.
#[cfg(feature = "std")]
#[derive(Debug)]
pub enum LoadError {
    Io(std::path::PathBuf, std::io::Error),
    Bad(std::path::PathBuf, ConfigError),
}

#[cfg(feature = "std")]
impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Bad(path, ConfigError::BadValue { line, key, detail }) => {
                write!(f, "{}:{line}: {key}: {detail}", path.display())
            }
        }
    }
}

#[cfg(feature = "std")]
impl core::error::Error for LoadError {}

/// Load `home/config`. A missing file is an empty config, not an error.
#[cfg(feature = "std")]
pub fn load(home: &std::path::Path) -> Result<Config, LoadError> {
    let path = home.join(CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => return Err(LoadError::Io(path, error)),
    };
    parse(&text).map_err(|error| LoadError::Bad(path, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(text: &str) -> Result<Config, ConfigError> {
        parse(text)
    }

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = load(dir.path()).expect("no file is fine");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn the_propagation_node_key_parses_with_comments_and_quotes() {
        let config = parse_str(
            "# the mailbox\npropagation_node = \"76cff79ab2f43bebcb7398e08e56ee0f\"  # note\n",
        )
        .expect("well-formed config");
        assert_eq!(
            config.propagation_node.map(|h| crate::address::to_hex(&h)),
            Some("76cff79ab2f43bebcb7398e08e56ee0f".to_string())
        );
    }

    /// The failure mode this module's header names: a typo in the mailbox
    /// hash must stop the run and say where, not quietly select nothing.
    #[test]
    fn a_malformed_node_hash_is_an_error_naming_the_line() {
        let error = parse_str("\npropagation_node = not-a-hash\n")
            .expect_err("a bad hash must not be ignored");
        assert!(
            matches!(
                error,
                ConfigError::BadValue {
                    line: 2,
                    key: "propagation_node",
                    ..
                }
            ),
            "the error must name line and key: {error:?}"
        );
    }

    /// The file half of the same rule: the shell adds the path, in the
    /// `file:line:` shape an editor jumps to.
    #[test]
    fn a_malformed_file_is_an_error_naming_file_line_and_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CONFIG_FILE);
        std::fs::write(&path, "\npropagation_node = not-a-hash\n").expect("write");
        let text = load(dir.path())
            .expect_err("a bad hash must not be ignored")
            .to_string();
        assert!(
            text.starts_with(&format!("{}:2: propagation_node: ", path.display())),
            "the error must name file, line and key: {text}"
        );
    }

    #[test]
    fn send_on_enter_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").send_on_enter, None);
        let off = parse_str("send_on_enter = false\n").expect("off");
        assert_eq!(off.send_on_enter, Some(false));
        let on = parse_str("send_on_enter = yes\n").expect("on");
        assert_eq!(on.send_on_enter, Some(true));
        assert!(matches!(
            parse_str("send_on_enter = maybe\n"),
            Err(ConfigError::BadValue {
                line: 1,
                key: "send_on_enter",
                ..
            })
        ));
    }

    #[test]
    fn theme_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").theme, None);
        for (text, flag) in [
            ("theme = auto\n", ThemeFlag::Auto),
            ("theme = dark\n", ThemeFlag::Dark),
            ("theme = \"light\"\n", ThemeFlag::Light),
        ] {
            assert_eq!(parse_str(text).expect(text).theme, Some(flag), "{text}");
        }
        assert!(matches!(
            parse_str("\ntheme = solarized\n"),
            Err(ConfigError::BadValue {
                line: 2,
                key: "theme",
                ..
            })
        ));
    }

    #[test]
    fn sync_interval_parses_and_a_typo_is_an_error() {
        assert_eq!(parse_str("").expect("empty").sync_interval, None);
        let half = parse_str("sync_interval = 30m\n").expect("30m");
        assert_eq!(half.sync_interval, Some(Some(30 * 60_000)));
        let off = parse_str("sync_interval = \"off\"\n").expect("off");
        assert_eq!(off.sync_interval, Some(None));
        assert!(matches!(
            parse_str("\nsync_interval = often\n"),
            Err(ConfigError::BadValue {
                line: 2,
                key: "sync_interval",
                ..
            })
        ));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let config = parse_str("future_key = whatever\n").expect("unknown keys pass");
        assert_eq!(config, Config::default());
    }
}
