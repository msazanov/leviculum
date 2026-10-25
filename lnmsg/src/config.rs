//! `lnmsg`'s own tiny config file.
//!
//! `${LNMSG_HOME}/config` (the same directory as the identity — see
//! [`crate::identity::home_dir`]) holds `key = value` lines. The one key this
//! slice reads is `propagation_node`, the persisted default for `--pn`:
//!
//! ```text
//! # the mailbox this machine uses when a direct delivery fails
//! propagation_node = 76cff79ab2f43bebcb7398e08e56ee0f
//! ```
//!
//! Deliberately not the Reticulum config file: that one belongs to the
//! daemon and is shared with Python's `rnsd`, and a foreign key in it would
//! be a compatibility hazard. And deliberately hand-parsed rather than a
//! TOML dependency: two keys a decade from now do not justify a parser
//! crate in a messenger's supply chain.
//!
//! A missing file means defaults. A present-but-wrong value is an error
//! that names the file and line, never silently ignored: a typo in a
//! mailbox hash would otherwise turn into "my mail goes nowhere" with no
//! trace.

use alloc::string::{String, ToString};

use crate::address;

/// The config file's name under the lnmsg home directory.
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
        // Unknown keys pass silently: a config written by a newer lnmsg
        // must not stop an older one from sending at all.
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
    fn unknown_keys_are_ignored() {
        let config = parse_str("future_key = whatever\n").expect("unknown keys pass");
        assert_eq!(config, Config::default());
    }
}
