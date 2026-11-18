use friday_engine::{Failure, PortFuture};
use secrecy::SecretString;

pub trait CredentialSource: Send + Sync {
    fn read_key(&self) -> PortFuture<'_, Option<SecretString>>;
    fn revision(&self) -> u64;
}

pub fn validate_key(value: &str) -> Result<(), Failure> {
    if (8..=512).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(())
    } else {
        Err(Failure::InvalidInput)
    }
}
