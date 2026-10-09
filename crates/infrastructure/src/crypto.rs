//! Tokens, hashing, HMAC and argon2id passwords (spec §4.5, §15.3).
//! Raw tokens are only ever returned to the caller; stores keep hashes.

use base64::Engine;
use contracts::AppError;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Minimum/maximum password length (spec §4.5).
pub const PASSWORD_MIN_LEN: usize = 12;
pub const PASSWORD_MAX_LEN: usize = 128;

/// 256-bit random token, base64url-encoded. Suitable for session, invitation,
/// manage and unsubscribe capabilities.
pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    B64.encode(buf)
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha256_bytes(data))
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

pub fn hmac_sha256_verify(key: &[u8], data: &[u8], expected: &[u8]) -> bool {
    let actual = hmac_sha256(key, data);
    actual.as_slice() == expected
}

pub fn b64_encode(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    B64.decode(s).ok()
}

/// argon2id m=19456 t=2 p=1 (spec §4.5). Returns the PHC string.
pub fn hash_password(password: &str) -> Result<String, AppError> {
    use argon2::password_hash::PasswordHasher;
    let params = argon2::Params::new(19_456, 2, 1, None)
        .map_err(|e| AppError::internal(format!("argon2 params: {e}")))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut salt = [0u8; 16];
    rand::rng().fill_bytes(&mut salt);
    argon
        .hash_password_with_salt(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::internal(format!("password hashing failed: {e}")))
}

/// Constant-time verify; returns false on malformed stored hash.
pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    use argon2::password_hash::PasswordVerifier;
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), stored_hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse battery").unwrap();
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("wrong", &h));
    }

    #[test]
    fn token_is_256_bits() {
        let t = generate_token();
        assert_eq!(B64.decode(&t).unwrap().len(), 32);
    }
}
