//! approvals module job handlers (spec §6 — module worker owned).
//! Approvals has no own job kinds: decision side effects are enqueued on
//! *other* modules' kinds (`campaigns.activate_launch`,
//! `sending.dispatch_outbound`, `booking.apply_change`,
//! `compliance.release_suppression`) inside the decision transaction, and
//! approval expiry is derived from `expires_at` at decision time — no
//! sweeper is required by A.6.3.

use crate::JobRegistry;

/// Register this module's JobHandlers on the worker registry.
pub fn register(_registry: &mut JobRegistry) {}
