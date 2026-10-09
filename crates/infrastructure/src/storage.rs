//! Encrypted object store for message bodies / import files (spec §15.3).
//! Default dev implementation is local filesystem + AES-256-GCM; S3/GCS
//! adapters implement the same trait later.
//!
//! `signed_url` issues an HMAC token bound to (key, expiry) — never raw
//! local paths. Downloads stay API-mediated.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use async_trait::async_trait;
use contracts::AppError;
use rand::RngCore;

use crate::crypto;

#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Store `bytes` (already encrypted by the impl) under opaque `key`.
    async fn put(&self, key: &str, plaintext: &[u8]) -> Result<(), AppError>;
    /// Fetch and decrypt.
    async fn get(&self, key: &str) -> Result<Vec<u8>, AppError>;
    async fn delete(&self, key: &str) -> Result<(), AppError>;
    /// Opaque capability token for API-mediated download (not a URL path).
    fn sign_access(&self, key: &str, ttl: Duration) -> String;
    /// Verify a token produced by `sign_access`.
    fn verify_access(&self, key: &str, token: &str) -> bool;
}

/// Local filesystem store, AES-256-GCM at rest.
pub struct LocalObjectStore {
    root: PathBuf,
    enc_key: [u8; 32],
    sign_key: Vec<u8>,
}

impl LocalObjectStore {
    pub fn new(root: impl AsRef<Path>, enc_key: [u8; 32], sign_key: Vec<u8>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            enc_key,
            sign_key,
        }
    }

    fn path_for(&self, key: &str) -> Result<PathBuf, AppError> {
        // Keys are opaque ids (uuid-ish); reject traversal defensively.
        if key.is_empty() || key.contains("..") || key.contains('/') || key.contains('\\') {
            return Err(AppError::validation("invalid object key", vec![]));
        }
        Ok(self.root.join(key))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new_from_slice(&self.enc_key).expect("32-byte key")
    }
}

#[async_trait]
impl ObjectStore for LocalObjectStore {
    async fn put(&self, key: &str, plaintext: &[u8]) -> Result<(), AppError> {
        let path = self.path_for(key)?;
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| AppError::internal(format!("object dir: {e}")))?;
        let mut nonce = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce);
        let ct = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|e| AppError::internal(format!("encrypt: {e}")))?;
        let mut buf = Vec::with_capacity(12 + ct.len());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ct);
        tokio::fs::write(path, buf)
            .await
            .map_err(|e| AppError::internal(format!("object write: {e}")))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, AppError> {
        let path = self.path_for(key)?;
        let buf = tokio::fs::read(path)
            .await
            .map_err(|_| AppError::not_found("object not found"))?;
        if buf.len() < 12 {
            return Err(AppError::internal("corrupt object"));
        }
        let (nonce, ct) = buf.split_at(12);
        self.cipher()
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| AppError::internal("object decrypt failed"))
    }

    async fn delete(&self, key: &str) -> Result<(), AppError> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::internal(format!("object delete: {e}"))),
        }
    }

    fn sign_access(&self, key: &str, ttl: Duration) -> String {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .saturating_add(ttl)
            .as_secs();
        let body = format!("{key}.{exp}");
        let sig = crypto::b64_encode(&crypto::hmac_sha256(&self.sign_key, body.as_bytes()));
        format!("{body}.{sig}")
    }

    fn verify_access(&self, key: &str, token: &str) -> bool {
        let mut parts = token.split('.');
        let (Some(k), Some(exp_s), Some(sig)) = (parts.next(), parts.next(), parts.next()) else {
            return false;
        };
        if parts.next().is_some() || k != key {
            return false;
        }
        let Ok(exp) = exp_s.parse::<u64>() else {
            return false;
        };
        let body = format!("{k}.{exp}");
        let Some(sig_bytes) = crypto::b64_decode(sig) else {
            return false;
        };
        if !crypto::hmac_sha256_verify(&self.sign_key, body.as_bytes(), &sig_bytes) {
            return false;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        exp >= now
    }
}
