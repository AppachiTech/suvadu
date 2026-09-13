//! Per-install keyed hash for `OpenCode` prompt reconciliation.
//!
//! Linking a command to its imported prompt across possibly-different
//! per-directory redaction policies needs a stable fingerprint of the
//! prompt's original text. A bare, unkeyed hash of that text would let
//! anyone who can read the local database or prompt cache offline
//! dictionary-guess a redacted secret by hashing candidates and comparing
//! -- exactly the class of low-entropy secret redaction exists to protect.
//! An HMAC keyed with a random, owner-only-readable per-install secret
//! closes that: guessing a match also requires compromising this key file.
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256 of `text` under this install's reconciliation key, hex
/// encoded. Returns `None` (not an error) if the key can't be loaded or
/// generated -- callers already treat a missing hash as "fall back to
/// exact-text match", the behavior before this fingerprint existed.
pub fn keyed_text_hash(text: &str) -> Option<String> {
    let key = reconcile_key().ok()?;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&key).ok()?;
    mac.update(text.as_bytes());
    Some(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
}

/// Load this install's reconciliation key, generating and persisting a
/// random one (owner-only permissions) on first use.
fn reconcile_key() -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let dirs = super::project_dirs().ok_or("Could not determine data directory")?;
    let key_path = dirs.data_dir().join("reconcile.key");
    if let Some(key) = read_key(&key_path) {
        return Ok(key);
    }
    std::fs::create_dir_all(dirs.data_dir())?;
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&key_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&key)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(key)
        }
        // Another process won the race to create it first; use its key
        // instead of the one just generated here, so both sides of a
        // reconciliation always end up hashing under the same key.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_key(&key_path).ok_or_else(|| e.into())
        }
        Err(e) => Err(e.into()),
    }
}

fn read_key(path: &std::path::Path) -> Option<[u8; 32]> {
    let bytes = std::fs::read(path).ok()?;
    <[u8; 32]>::try_from(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_text_hash_is_deterministic_and_persists_the_key_across_calls() {
        let a = keyed_text_hash("hello").unwrap();
        let b = keyed_text_hash("hello").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, keyed_text_hash("different text").unwrap());
    }

    #[test]
    fn keyed_text_hash_is_not_a_bare_unkeyed_hash() {
        use sha2::Digest;
        let unkeyed = format!("sha256:{:x}", Sha256::digest(b"hello"));
        assert_ne!(keyed_text_hash("hello").unwrap(), unkeyed);
    }
}
