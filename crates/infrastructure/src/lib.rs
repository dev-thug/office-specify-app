//! Infrastructure: Db/Tx + RLS, repositories, audit, outbox, jobs,
//! idempotency, crypto, cursor, rate limiting, providers, storage, secrets,
//! Redis, config, test fixtures (spec §4).

pub mod audit;
pub mod config;
pub mod crypto;
pub mod cursor;
pub mod db;
pub mod error;
pub mod idempotency;
pub mod jobs;
pub mod outbox;
pub mod providers;
pub mod public_tokens;
pub mod ratelimit;
pub mod redis;
pub mod repo;
pub mod retention;
pub mod secrets;
pub mod storage;
/// Dev/test fixtures — behind the `testing` cargo feature so release
/// builds cannot ship them (R2).
#[cfg(feature = "testing")]
pub mod testing;

pub use db::{Db, Tx};
