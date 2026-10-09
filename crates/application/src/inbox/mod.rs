//! inbox module use cases (spec §6 — owned by the module worker).
//!
//! - `threads`: list / get / assignment / read-state / patch / reply-drafts
//! - `reviews`: human classification review (§9.4 safety stop)
//! - `attachments`: §15.2 download capability issuance + serving
//! - `ingest`: provider message → thread/message upsert + classification +
//!   `message.received` + `campaigns.pause_address` (§9.2/§9.4)
//! - `sync`: `inbox.sync_mailbox` / `inbox.process_webhook` bodies
//! - `webhooks`: §14.3 ingress (signature verify → dedupe → enqueue)

pub mod attachments;
pub mod ingest;
pub mod reviews;
pub mod sync;
pub mod threads;
pub mod webhooks;
