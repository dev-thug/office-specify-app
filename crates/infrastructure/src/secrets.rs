//! Secret store abstraction (spec §15.3). Default dev impl: AES-256-GCM
//! encrypted JSON file; prod swaps to an external KMS-backed impl behind the
//! same trait. Secrets never appear in logs or API responses.

use std::collections::BTreeMap;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use contracts::AppError;
use rand::RngCore;
use serde::{Deserialize, Serialize};

#[async_trait::async_trait]
pub trait SecretStore: Send + Sync {
    /// Store `plaintext` under `name` (e.g. `provider_account:{uuid}`).
    async fn put(&self, name: &str, plaintext: &str) -> Result<(), AppError>;
    /// Returns None when absent.
    async fn get(&self, name: &str) -> Result<Option<String>, AppError>;
    async fn delete(&self, name: &str) -> Result<(), AppError>;
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    secrets: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    nonce_b64: String,
    ct_b64: String,
}

pub struct FileSecretStore {
    path: PathBuf,
    key: [u8; 32],
    lock: tokio::sync::Mutex<()>,
}

impl FileSecretStore {
    pub fn new(path: impl Into<PathBuf>, key: [u8; 32]) -> Self {
        Self {
            path: path.into(),
            key,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new_from_slice(&self.key).expect("32-byte key")
    }

    async fn load(&self) -> Result<StoreFile, AppError> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| AppError::internal(format!("secret store corrupt: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile::default()),
            Err(e) => Err(AppError::internal(format!("secret store read: {e}"))),
        }
    }

    async fn save(&self, file: &StoreFile) -> Result<(), AppError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::internal(format!("secret store dir: {e}")))?;
        }
        let bytes = serde_json::to_vec(file)
            .map_err(|e| AppError::internal(format!("secret store encode: {e}")))?;
        tokio::fs::write(&self.path, bytes)
            .await
            .map_err(|e| AppError::internal(format!("secret store write: {e}")))?;
        // Best-effort 0600 on unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .await;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl SecretStore for FileSecretStore {
    async fn put(&self, name: &str, plaintext: &str) -> Result<(), AppError> {
        let _g = self.lock.lock().await;
        let mut file = self.load().await?;
        let mut nonce = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce);
        let ct = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .map_err(|e| AppError::internal(format!("secret encrypt: {e}")))?;
        file.secrets.insert(
            name.to_string(),
            Entry {
                nonce_b64: crate::crypto::b64_encode(&nonce),
                ct_b64: crate::crypto::b64_encode(&ct),
            },
        );
        self.save(&file).await
    }

    async fn get(&self, name: &str) -> Result<Option<String>, AppError> {
        let _g = self.lock.lock().await;
        let file = self.load().await?;
        let Some(entry) = file.secrets.get(name) else {
            return Ok(None);
        };
        let nonce = crate::crypto::b64_decode(&entry.nonce_b64)
            .ok_or_else(|| AppError::internal("secret nonce decode"))?;
        let ct = crate::crypto::b64_decode(&entry.ct_b64)
            .ok_or_else(|| AppError::internal("secret ct decode"))?;
        let pt = self
            .cipher()
            .decrypt(Nonce::from_slice(&nonce), ct.as_slice())
            .map_err(|_| AppError::internal("secret decrypt failed"))?;
        String::from_utf8(pt)
            .map(Some)
            .map_err(|_| AppError::internal("secret not utf8"))
    }

    async fn delete(&self, name: &str) -> Result<(), AppError> {
        let _g = self.lock.lock().await;
        let mut file = self.load().await?;
        file.secrets.remove(name);
        self.save(&file).await
    }
}
