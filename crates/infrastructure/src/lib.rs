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
pub mod ratelimit;
pub mod redis;
pub mod repo;
pub mod secrets;
pub mod storage;
pub mod testing;

pub use db::{Db, Tx};
