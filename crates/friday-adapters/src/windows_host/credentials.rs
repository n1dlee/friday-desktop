use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use friday_engine::{Failure, PortFuture};
use keyring_core::{Entry, Error, api::CredentialStoreApi};
use secrecy::{ExposeSecret, SecretString};
use windows_native_keyring_store::Store;

use crate::{CredentialSource, validate_key};

const SERVICE: &str = "io.github.n1dlee.friday-desktop";
const ACCOUNT: &str = "groq";

#[derive(Default)]
pub struct WindowsCredentials {
    revision: Arc<AtomicU64>,
}

impl WindowsCredentials {
    fn entry() -> Result<Entry, Failure> {
        Store::new()
            .and_then(|store| store.build(SERVICE, ACCOUNT, None))
            .map_err(|_| Failure::CredentialStore)
    }

    pub async fn present(&self) -> Result<bool, Failure> {
        self.read_key().await.map(|value| value.is_some())
    }

    pub async fn set(&self, key: SecretString) -> Result<(), Failure> {
        validate_key(key.expose_secret())?;
        let revision = self.revision.clone();
        tokio::task::spawn_blocking(move || {
            Self::entry()?
                .set_password(key.expose_secret())
                .map_err(|_| Failure::CredentialStore)?;
            revision.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
        .await
        .map_err(|_| Failure::CredentialStore)?
    }

    pub async fn delete(&self) -> Result<(), Failure> {
        let revision = self.revision.clone();
        tokio::task::spawn_blocking(move || match Self::entry()?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => {
                revision.fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            Err(_) => Err(Failure::CredentialStore),
        })
        .await
        .map_err(|_| Failure::CredentialStore)?
    }
}

impl CredentialSource for WindowsCredentials {
    fn read_key(&self) -> PortFuture<'_, Option<SecretString>> {
        Box::pin(async {
            tokio::task::spawn_blocking(|| match Self::entry()?.get_password() {
                Ok(key) => Ok(Some(SecretString::from(key))),
                Err(Error::NoEntry) => Ok(None),
                Err(_) => Err(Failure::CredentialStore),
            })
            .await
            .map_err(|_| Failure::CredentialStore)?
        })
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
}
