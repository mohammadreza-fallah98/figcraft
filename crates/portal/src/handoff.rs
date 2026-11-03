//! The note a login leaves for the other copy of the app.
//!
//! When the browser follows `figcraft://auth?code=…`, the system starts a *new* FigCraft process
//! with that link (a running copy doesn't receive it). That copy has never seen the verifier, so
//! the copy that pressed Log in leaves a note in the app's settings folder: the verifier (useless
//! without the one-time code) and when it was made. The new copy reads it, trades the code, keeps
//! the refresh token in the credential store and rewrites the note as `done` (or `failed` with the
//! portal's message); the first copy sees that and signs in from the stored refresh token.
//! No token is ever in the note.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The note's file name in the settings folder.
pub const FILE: &str = "portal-login.txt";

/// What the note says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// A login is waiting for its callback, started at `started` (seconds since 1970).
    Waiting { verifier: String, started: u64 },
    /// The callback was handled by another copy: the session is in the credential store.
    Done,
    /// The callback was handled by another copy and failed, with this message.
    Failed(String),
}

impl Note {
    /// The note as written to the file.
    pub fn render(&self) -> String {
        match self {
            Note::Waiting { verifier, started } => format!("waiting\n{verifier}\n{started}\n"),
            Note::Done => "done\n".to_string(),
            Note::Failed(message) => format!("failed\n{}\n", message.chars().filter(|c| !c.is_control()).take(200).collect::<String>()),
        }
    }

    /// Read a note back; anything that isn't one is `None`.
    pub fn parse(text: &str) -> Option<Note> {
        let mut lines = text.lines();
        match lines.next()? {
            "waiting" => {
                let verifier = lines.next()?.to_string();
                let started = lines.next()?.trim().parse().ok()?;
                crate::pkce::is_valid_verifier(&verifier).then_some(Note::Waiting { verifier, started })
            }
            "done" => Some(Note::Done),
            "failed" => Some(Note::Failed(lines.next().unwrap_or_default().to_string())),
            _ => None,
        }
    }
}

/// Seconds since 1970.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The note in `dir`, if there is a readable one.
pub fn read(dir: &Path) -> Option<Note> {
    Note::parse(&std::fs::read_to_string(dir.join(FILE)).ok()?)
}

/// Leave `note` in `dir`.
pub fn write(dir: &Path, note: &Note) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(FILE), note.render()).map_err(|e| e.to_string())
}

/// Take the note away (none is fine).
pub fn clear(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(FILE));
}
