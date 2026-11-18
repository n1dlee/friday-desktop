#![deny(unsafe_code)]

mod credentials;
mod groq;
mod sqlite;

pub use credentials::{CredentialSource, validate_key};
pub use groq::Groq;
pub use sqlite::SqliteStore;

#[cfg(any(windows, test))]
mod pcm;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows_host;
