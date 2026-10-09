//! HMAC-signed opaque pagination cursors (spec §4.4, B.1.2). The cursor
//! binds the sort values, the request's filter/sort projection hash and the
//! issuing principal — a cursor minted for one filter or actor is rejected
//! for another.

use contracts::AppError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto;

const CURSOR_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    v: u8,
    /// Keyset values after the last row (e.g. [created_at, id]).
    pub sort_values: Vec<serde_json::Value>,
    /// sha256 hex of canonical {filters, sort} — caller-computed.
    pub filter_hash: String,
    /// Principal that requested the page.
    pub principal: Uuid,
}

impl Cursor {
    pub fn new(sort_values: Vec<serde_json::Value>, filter_hash: String, principal: Uuid) -> Self {
        Self {
            v: CURSOR_VERSION,
            sort_values,
            filter_hash,
            principal,
        }
    }

    /// sha256 hex helper for building `filter_hash` from canonical JSON.
    pub fn hash_filter(filters: &serde_json::Value) -> String {
        crypto::sha256_hex(filters.to_string().as_bytes())
    }

    pub fn encode(&self, hmac_key: &[u8]) -> String {
        let body = crypto::b64_encode(&serde_json::to_vec(self).unwrap_or_else(|_| b"{}".to_vec()));
        let sig = crypto::b64_encode(&crypto::hmac_sha256(hmac_key, body.as_bytes()));
        format!("{body}.{sig}")
    }

    pub fn decode(
        encoded: &str,
        hmac_key: &[u8],
        principal: Uuid,
        expected_filter_hash: &str,
    ) -> Result<Self, AppError> {
        let invalid = || AppError::validation("invalid pagination cursor", vec![]);
        let (body, sig) = encoded.split_once('.').ok_or_else(invalid)?;
        let sig_bytes = crypto::b64_decode(sig).ok_or_else(invalid)?;
        if !crypto::hmac_sha256_verify(hmac_key, body.as_bytes(), &sig_bytes) {
            return Err(invalid());
        }
        let raw = crypto::b64_decode(body).ok_or_else(invalid)?;
        let cur: Cursor = serde_json::from_slice(&raw).map_err(|_| invalid())?;
        if cur.v != CURSOR_VERSION {
            return Err(invalid());
        }
        if cur.principal != principal || cur.filter_hash != expected_filter_hash {
            return Err(invalid());
        }
        Ok(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_roundtrip_and_binding() {
        let key = b"k";
        let p = Uuid::new_v4();
        let c = Cursor::new(
            vec![
                serde_json::json!("2026-01-01T00:00:00Z"),
                serde_json::json!(Uuid::new_v4()),
            ],
            "fh".into(),
            p,
        );
        let enc = c.encode(key);
        assert!(Cursor::decode(&enc, key, p, "fh").is_ok());
        // tampered / wrong principal / wrong filter
        assert!(Cursor::decode(&enc, key, Uuid::new_v4(), "fh").is_err());
        assert!(Cursor::decode(&enc, key, p, "other").is_err());
        assert!(Cursor::decode(&enc, b"other-key", p, "fh").is_err());
        assert!(Cursor::decode("garbage", key, p, "fh").is_err());
    }
}
