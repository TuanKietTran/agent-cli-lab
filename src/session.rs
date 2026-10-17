//! Named sessions (spec 007): `host --name NAME` and `-n NAME` instead of an absolute socket path,
//! plus a small registry so that `list` can show what is running.
//!
//! The name is only a convenience. The control socket is still the single source of truth for
//! whether a host is alive, so a stale registry file is never trusted: `list` checks the socket.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// Longest name accepted. A Unix socket path is limited to about 100 bytes, and the directory
/// already uses some of them.
const MAX_NAME: usize = 40;

/// What `host --name` records about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Process id of the host (not of the hosted program).
    pub pid: u32,
    pub socket: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub started_ms: u64,
}

/// A name is a file name and a socket name, so it stays to plain characters.
pub fn validate_name(name: &str) -> Result<()> {
    let plain = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.starts_with('.');
    if plain {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid session name {name:?}: use 1 to {MAX_NAME} letters, digits, '.', '_' or '-', not starting with '.'"
        ))
    }
}

/// Directory of the sockets and registry files. `AGENT_CLI_LAB_DIR` overrides it.
pub fn session_dir() -> PathBuf {
    session_dir_from(
        env::var_os("AGENT_CLI_LAB_DIR"),
        env::var_os("LOCALAPPDATA"),
        env::var_os("XDG_RUNTIME_DIR"),
        env::var_os("TMPDIR"),
        env::temp_dir(),
    )
}

fn session_dir_from(
    explicit: Option<OsString>,
    local_app_data: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    tmpdir: Option<OsString>,
    system_temp: PathBuf,
) -> PathBuf {
    if let Some(path) = explicit.filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    if cfg!(windows)
        && let Some(base) = local_app_data.filter(|value| !value.is_empty())
    {
        return PathBuf::from(base).join("agent-cli-lab").join("sessions");
    }
    let base = xdg_runtime_dir
        .filter(|value| !value.is_empty())
        .or(tmpdir.filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .unwrap_or(system_temp);
    base.join("agent-cli-lab")
}

/// The control socket of the session called `name`.
pub fn socket_for(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(session_dir().join(format!("{name}.sock")))
}

fn entry_path(directory: &Path, name: &str) -> PathBuf {
    directory.join(format!("{name}.json"))
}

/// Removes the registry file when dropped, so a host that exits normally leaves nothing behind.
pub struct Registration {
    path: PathBuf,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn register(entry: &Entry) -> Result<Registration> {
    register_in(&session_dir(), entry)
}

fn register_in(directory: &Path, entry: &Entry) -> Result<Registration> {
    validate_name(&entry.name)?;
    fs::create_dir_all(directory)
        .with_context(|| format!("failed to create session directory {}", directory.display()))?;
    let path = entry_path(directory, &entry.name);
    // Write beside the final name and rename, so `list` never reads half a file.
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(entry)?)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Registration { path })
}

/// Every registry file that parses, ordered by start time. Damaged files are skipped.
pub fn entries() -> Vec<Entry> {
    entries_in(&session_dir())
}

fn entries_in(directory: &Path) -> Vec<Entry> {
    let mut found: Vec<Entry> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|item| {
            item.path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|item| fs::read(item.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Entry>(&bytes).ok())
        .filter(|entry| validate_name(&entry.name).is_ok())
        .collect();
    found.sort_by_key(|entry| (entry.started_ms, entry.name.clone()));
    found
}

/// Deletes the registry file of `name` (used for a host that no longer answers).
pub fn forget(name: &str) {
    if validate_name(name).is_ok() {
        let _ = fs::remove_file(entry_path(&session_dir(), name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, started_ms: u64) -> Entry {
        Entry {
            name: name.into(),
            pid: 4242,
            socket: format!("/s/{name}.sock"),
            command: vec!["pi".into(), "--no-session".into()],
            cwd: Some("/work".into()),
            title: None,
            cols: 150,
            rows: 40,
            started_ms,
        }
    }

    #[test]
    fn names_are_plain() {
        for good in ["ux", "core-lab", "a.b_c-1", &"x".repeat(MAX_NAME)] {
            assert!(validate_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ".hidden",
            "a/b",
            "a\\b",
            "a b",
            "../x",
            "é",
            &"x".repeat(MAX_NAME + 1),
        ] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn directory_precedence() {
        let explicit = Some(OsString::from("/explicit"));
        assert_eq!(
            session_dir_from(
                explicit,
                Some("C:\\L".into()),
                Some("/xdg".into()),
                None,
                "/sys".into()
            ),
            PathBuf::from("/explicit")
        );
        assert_eq!(
            session_dir_from(
                None,
                None,
                Some("/xdg".into()),
                Some("/tmpdir".into()),
                "/sys".into()
            ),
            PathBuf::from("/xdg/agent-cli-lab")
        );
        assert_eq!(
            session_dir_from(
                None,
                None,
                Some("".into()),
                Some("/tmpdir".into()),
                "/sys".into()
            ),
            PathBuf::from("/tmpdir/agent-cli-lab")
        );
        assert_eq!(
            session_dir_from(None, None, None, None, "/sys".into()),
            PathBuf::from("/sys/agent-cli-lab")
        );
        if cfg!(windows) {
            assert_eq!(
                session_dir_from(
                    None,
                    Some("C:\\Local".into()),
                    None,
                    None,
                    "C:\\Temp".into()
                ),
                PathBuf::from("C:\\Local")
                    .join("agent-cli-lab")
                    .join("sessions")
            );
        }
    }

    #[test]
    fn registration_round_trips_and_cleans_up() {
        let directory = tempfile::tempdir().unwrap();
        let first = register_in(directory.path(), &entry("ux", 2)).unwrap();
        let second = register_in(directory.path(), &entry("core", 1)).unwrap();
        let listed = entries_in(directory.path());
        assert_eq!(
            listed.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["core", "ux"]
        );
        assert_eq!(listed[1], entry("ux", 2));
        drop(first);
        assert_eq!(entries_in(directory.path()).len(), 1);
        drop(second);
        assert!(entries_in(directory.path()).is_empty());
        assert!(
            fs::read_dir(directory.path()).unwrap().next().is_none(),
            "no temporary file is left"
        );
    }

    #[test]
    fn damaged_or_foreign_files_are_ignored() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("broken.json"), b"{ not json").unwrap();
        fs::write(directory.path().join("notes.txt"), b"hello").unwrap();
        let mut hostile = entry("ok", 1);
        hostile.name = "../escape".into();
        fs::write(
            directory.path().join("hostile.json"),
            serde_json::to_vec(&hostile).unwrap(),
        )
        .unwrap();
        let _kept = register_in(directory.path(), &entry("ok", 1)).unwrap();
        let listed = entries_in(directory.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "ok");
    }

    #[test]
    fn a_name_that_is_not_plain_cannot_be_registered() {
        let directory = tempfile::tempdir().unwrap();
        assert!(register_in(directory.path(), &entry("a/b", 1)).is_err());
    }
}
