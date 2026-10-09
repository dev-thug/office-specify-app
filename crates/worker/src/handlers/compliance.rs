//! compliance module job handlers (spec §6 — owned by the module worker).

use crate::JobRegistry;

/// Register this module's JobHandlers on the worker registry.
pub fn register(_registry: &mut JobRegistry) {}
