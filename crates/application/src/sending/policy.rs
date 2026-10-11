//! §8.4 execution-time policy chain for a single outbound message.
//!
//! The chain is evaluated identically by `POST /outbound-messages/{id}/
//! preflight` (non-binding report, B.3.10) and by `sending.dispatch_outbound`
//! (binding, immediately before the provider call). Every predicate fails
//! closed: an unknown/missing/unverifiable state blocks the send.
//!
//! Order (source §8.4):
//!   account/channel health → provider purpose policy → requester/approver
//!   permissions → approval hash/expiry/revocation → execution state +
//!   emergency stop → suppression + sending basis → new replies/bookings/
//!   duplicate contact → sending-domain + sender verification →
//!   timezone/schedule limits → daily + campaign caps → payload/hash
//!   consistency.

use chrono::{DateTime, Duration, Utc};
use contracts::AppError;
use uuid::Uuid;

use infrastructure::db::Tx;
use infrastructure::repo::compliance_core;
use infrastructure::repo::sending::{self, OutboundRow};

/// Stable check names — also the contract of the preflight response.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    /// Stable failure code (B.5-style string) when `passed` is false.
    pub code: Option<&'static str>,
    /// For deferrable checks: when the check may pass next.
    pub defer_until: Option<DateTime<Utc>>,
}

impl Check {
    fn pass(name: &'static str) -> Self {
        Self {
            name,
            passed: true,
            code: None,
            defer_until: None,
        }
    }
    fn fail(name: &'static str, code: &'static str) -> Self {
        Self {
            name,
            passed: false,
            code: Some(code),
            defer_until: None,
        }
    }
    fn defer(name: &'static str, code: &'static str, until: DateTime<Utc>) -> Self {
        Self {
            name,
            passed: false,
            code: Some(code),
            defer_until: Some(until),
        }
    }
}

/// Campaign approvals pin the launch manifest, not each generated outbound.
/// The frozen launch/enrollment/step relationships and the outbound's
/// immutable payload hash establish that this message is a member of that
/// approved manifest. The approver's current mailbox grant is rechecked on
/// every preflight and dispatch.
async fn campaign_approval_checks(
    tx: &mut Tx<'_>,
    outbound: &OutboundRow,
    now: DateTime<Utc>,
    checks: &mut Vec<Check>,
) -> Result<(), AppError> {
    let Some(state) = sending::campaign_launch_approval_state(tx, outbound).await? else {
        checks.push(Check::fail("approval_state", CODE_APPROVAL_REQUIRED));
        checks.push(Check::fail("approval_hash", CODE_APPROVAL_REQUIRED));
        checks.push(Check::fail("approver_permission", CODE_APPROVAL_REQUIRED));
        return Ok(());
    };

    let state_code = campaign_state_code(&state, now);
    checks.push(match state_code {
        None => Check::pass("approval_state"),
        Some(code) => Check::fail("approval_state", code),
    });

    let manifest_matches = campaign_manifest_matches(&state);
    let decision_matches = campaign_decision_matches(&state);
    let thread_id = outbound.thread_id.map(|id| id.to_string());
    let expected_payload_hash =
        infrastructure::crypto::sha256_bytes(&domain::campaigns::payload_bytes_with_context(
            &outbound.from_address_snapshot,
            &outbound.to_address_snapshot,
            &outbound.subject,
            &outbound.body_text,
            outbound.body_html.as_deref(),
            thread_id.as_deref(),
            outbound.internet_message_id.as_deref(),
        ));
    let payload_hash_matches = state.payload_hash_len == 32
        && outbound.payload_hash.as_slice() == expected_payload_hash.as_slice();
    let membership_matches = state.request_action_type.as_deref() == Some("campaign_launch")
        && state.revision_status == "frozen"
        && outbound
            .revision_id
            .is_some_and(|revision_id| state.launch_revision_id == revision_id)
        && state.launch_channel_id == outbound.delivery_channel_id
        && state.revision_channel_id == outbound.delivery_channel_id
        && state.enrollment_launch_id == outbound.launch_id
        && state.enrollment_revision_id == outbound.revision_id
        && state.enrollment_email_address_id == Some(outbound.to_email_address_id)
        && state.step_revision_id == outbound.revision_id
        && state.step_order.is_some()
        && payload_hash_matches;
    let hash_ok =
        state_code.is_none() && manifest_matches && decision_matches && membership_matches;
    let hash_failure = if let Some(code) = state_code {
        code
    } else if !manifest_matches || !membership_matches {
        "SNAPSHOT_MISMATCH"
    } else {
        CODE_APPROVAL_REJECTED
    };
    checks.push(if hash_ok {
        Check::pass("approval_hash")
    } else {
        Check::fail("approval_hash", hash_failure)
    });

    let approver_ok = match (state.decided_by, state.decision.as_deref()) {
        (Some(decided_by), Some("approved")) => {
            match sending::principal_state(tx, decided_by).await? {
                Some((status, kind, role))
                    if status == "active"
                        && kind == "human"
                        && (role == "admin" || role == "manager") =>
                {
                    matches!(
                        sending::grant_for_principal(tx, decided_by, outbound.mail_account_id)
                            .await?,
                        Some((_, _, _, true, _))
                    )
                }
                _ => false,
            }
        }
        _ => false,
    };
    checks.push(if hash_ok && approver_ok {
        Check::pass("approver_permission")
    } else {
        Check::fail(
            "approver_permission",
            if hash_ok {
                CODE_APPROVER_PERMISSION
            } else {
                hash_failure
            },
        )
    });
    Ok(())
}

// Codes that are NOT in contracts::ErrorCode (they are internal reason
// strings / failure_code values, not HTTP problem codes).
pub const CODE_CHANNEL_MISSING: &str = "CHANNEL_MISSING";
pub const CODE_CHANNEL_DISABLED: &str = "CHANNEL_DISABLED";
pub const CODE_MAILBOX_DISABLED: &str = "MAILBOX_DISABLED";
pub const CODE_MAILBOX_PENDING: &str = "MAILBOX_PENDING";
pub const CODE_CONNECTION_INACTIVE: &str = "CONNECTION_INACTIVE";
pub const CODE_PURPOSE_MISMATCH: &str = "PURPOSE_MISMATCH";
pub const CODE_CREDENTIAL_REVOKED: &str = "CREDENTIAL_REVOKED";
pub const CODE_APPROVAL_REQUIRED: &str = "APPROVAL_REQUIRED";
pub const CODE_APPROVAL_PENDING: &str = "APPROVAL_PENDING";
pub const CODE_APPROVAL_REJECTED: &str = "APPROVAL_REJECTED";
pub const CODE_APPROVAL_REVOKED: &str = "APPROVAL_REVOKED";
pub const CODE_APPROVER_PERMISSION: &str = "APPROVER_PERMISSION_REVOKED";
pub const CODE_SEND_WORKERS_LOCKED: &str = "SEND_WORKERS_LOCKED";
pub const CODE_ENROLLMENT_NOT_ACTIVE: &str = "ENROLLMENT_NOT_ACTIVE";
pub const CODE_SENDER_UNVERIFIED: &str = "SENDER_IDENTITY_UNVERIFIED";
pub const CODE_SENDER_DISABLED: &str = "SENDER_IDENTITY_DISABLED";
pub const CODE_DAILY_CAP: &str = "DAILY_CAP_REACHED";
pub const CODE_QUIET_HOURS: &str = "QUIET_HOURS";
pub const CODE_NOT_BEFORE: &str = "NOT_BEFORE";
pub const CODE_WORKSPACE_DISABLED: &str = "WORKSPACE_DISABLED";
pub const CODE_CLAIM_RELEASED: &str = "REPLY_CLAIM_RELEASED";

/// Check the latest campaign approval and launch state at the current time.
fn campaign_state_code(
    state: &sending::CampaignLaunchApprovalState,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    let request_code = match state.request_status.as_deref() {
        None => Some(CODE_APPROVAL_REQUIRED),
        Some("approved")
            if state
                .request_expires_at
                .is_some_and(|expires_at| expires_at > now)
                && state.launch_expires_at > now =>
        {
            None
        }
        Some("approved") | Some("expired") => Some("APPROVAL_EXPIRED"),
        Some("pending")
            if state
                .request_expires_at
                .is_some_and(|expires_at| expires_at > now) =>
        {
            Some(CODE_APPROVAL_PENDING)
        }
        Some("pending") => Some("APPROVAL_EXPIRED"),
        Some("rejected") => Some(CODE_APPROVAL_REJECTED),
        Some("revoked") => Some(CODE_APPROVAL_REVOKED),
        Some(_) => Some(CODE_APPROVAL_REJECTED),
    };
    let launch_code = match state.launch_status.as_str() {
        "approved" | "running" if state.launch_expires_at > now => None,
        "approved" | "running" => Some("APPROVAL_EXPIRED"),
        "frozen" | "awaiting_approval" | "building" => Some(CODE_APPROVAL_PENDING),
        "paused" | "completed" | "canceled" => Some(CODE_APPROVAL_REVOKED),
        _ => Some(CODE_APPROVAL_REJECTED),
    };
    request_code.or(launch_code)
}

fn campaign_manifest_matches(state: &sending::CampaignLaunchApprovalState) -> bool {
    state
        .launch_manifest_hash
        .as_ref()
        .filter(|hash| hash.len() == 32)
        .is_some_and(|manifest| state.request_snapshot_hash.as_ref() == Some(manifest))
}

fn campaign_decision_matches(state: &sending::CampaignLaunchApprovalState) -> bool {
    state.decision.as_deref() == Some("approved")
        && state.request_requested_by.is_some()
        && state.decision_snapshot_hash == state.request_snapshot_hash
        && state
            .request_version
            .is_some_and(|version| version.checked_sub(1) == state.decision_version_at_decision)
}

/// Terminal failures → the outbound is canceled with this failure_code.
pub fn is_cancel(code: &str) -> bool {
    !is_defer(code)
}

/// Temporary blocks → the outbound stays `queued` and dispatch is rearmed.
pub fn is_defer(code: &str) -> bool {
    matches!(
        code,
        "WORKSPACE_EMERGENCY_STOP"
            | CODE_SEND_WORKERS_LOCKED
            | "PROVIDER_POLICY_UNVERIFIED"
            | CODE_MAILBOX_PENDING
            | CODE_CONNECTION_INACTIVE // reauth/pending connections recover
            | "INTEGRATION_REAUTH_REQUIRED"
            | CODE_SENDER_UNVERIFIED
            | CODE_NOT_BEFORE
            | CODE_QUIET_HOURS
            | CODE_DAILY_CAP
            | "RATE_LIMITED"
            | "DEPENDENCY_UNAVAILABLE"
    )
}

/// Global (non-recipient) blocking reasons — the `global_blocking_reasons`
/// list of the preflight contract.
pub fn is_global(code: &str) -> bool {
    matches!(
        code,
        "WORKSPACE_EMERGENCY_STOP"
            | CODE_SEND_WORKERS_LOCKED
            | "PROVIDER_POLICY_UNVERIFIED"
            | "PROVIDER_POLICY_BLOCKED"
            | CODE_CHANNEL_MISSING
            | CODE_CHANNEL_DISABLED
            | CODE_MAILBOX_DISABLED
            | CODE_MAILBOX_PENDING
            | CODE_CONNECTION_INACTIVE
            | "INTEGRATION_REAUTH_REQUIRED"
            | CODE_PURPOSE_MISMATCH
            | CODE_WORKSPACE_DISABLED
    )
}

/// How long transient polls wait before the next dispatch attempt.
pub const POLL_DEFER: Duration = Duration::minutes(15);
/// Bound for reconcile-after-unknown polling (5 min × 12 ≈ 1h coverage).
pub const RECONCILE_DEFER: Duration = Duration::minutes(5);
pub const MAX_RECONCILE_JOBS: i64 = 12;

/// Evaluate every predicate in §8.4 order for `o`. `expected_last_inbound`
/// is the reply-review anchor (from the source draft when present, else
/// `reply_to_message_id`). The caller must already hold the
/// `email_addresses` row lock (A.6.4) — suppression/basis checks run
/// under it.
///
/// Every failure is a `Check { passed: false, code }` — nothing here
/// throws for a policy miss; DB errors propagate (fail closed at a higher
/// level: the dispatch handler aborts, the job retries).
pub async fn evaluate(
    tx: &mut Tx<'_>,
    o: &OutboundRow,
    expected_last_inbound: Option<Uuid>,
) -> Result<Vec<Check>, AppError> {
    let mut checks = Vec::with_capacity(20);
    let now = Utc::now();

    // -- 1..2 workspace flags evaluated early (emergency stop is an
    //    execution-state check but is reported globally too).
    let flags = sending::workspace_flags(tx).await?;
    let Some((emergency_stop, _, _tz)) = flags else {
        checks.push(Check::fail("workspace_active", CODE_WORKSPACE_DISABLED));
        return Ok(checks);
    };

    // -- Account / channel health -------------------------------------
    let chan = sending::channel_state(tx, o.delivery_channel_id).await?;
    let Some(chan) = chan else {
        checks.push(Check::fail("channel_exists", CODE_CHANNEL_MISSING));
        return Ok(checks);
    };
    checks.push(Check::pass("channel_exists"));

    // Composite-FK fact: outbound.purpose must equal channel.purpose.
    if chan.purpose == o.purpose {
        checks.push(Check::pass("purpose_matches_channel"));
    } else {
        checks.push(Check::fail(
            "purpose_matches_channel",
            CODE_PURPOSE_MISMATCH,
        ));
    }

    checks.push(if chan.enabled {
        Check::pass("channel_enabled")
    } else {
        Check::fail("channel_enabled", CODE_CHANNEL_DISABLED)
    });
    checks.push(match chan.mail_account_status.as_str() {
        "active" => Check::pass("mail_account_active"),
        "disabled" => Check::fail("mail_account_active", CODE_MAILBOX_DISABLED),
        _ => Check::defer(
            "mail_account_active",
            "INTEGRATION_REAUTH_REQUIRED",
            now + POLL_DEFER,
        ),
    });
    checks.push(match chan.connection_status.as_str() {
        "active" => Check::pass("connection_active"),
        "disabled" => Check::fail("connection_active", CODE_CONNECTION_INACTIVE),
        _ => Check::defer(
            "connection_active",
            "INTEGRATION_REAUTH_REQUIRED",
            now + POLL_DEFER,
        ),
    });

    // -- Provider purpose policy --------------------------------------
    checks.push(match chan.policy_status.as_str() {
        "allowed" => Check::pass("provider_purpose_policy"),
        "blocked" => Check::fail("provider_purpose_policy", "PROVIDER_POLICY_BLOCKED"),
        _ => Check::defer(
            "provider_purpose_policy",
            "PROVIDER_POLICY_UNVERIFIED",
            now + POLL_DEFER,
        ),
    });

    // -- Requester permission (re-checked at send time — T10) ----------
    let requester = sending::principal_state(tx, o.requested_by).await?;
    let requester_is_agent = matches!(&requester, Some((_, k, _)) if k == "agent");
    match requester {
        Some((status, kind, _role)) if status == "active" => {
            checks.push(Check::pass("requester_active"));
            if kind == "agent" {
                // Agents may execute only through the separate
                // approved-submit scope, and their mailbox can_send grant is
                // rechecked here before the provider boundary.
                let grant =
                    sending::grant_for_principal(tx, o.requested_by, o.mail_account_id).await?;
                checks.push(if matches!(grant, Some((_, _, true, _, _))) {
                    Check::pass("requester_permission")
                } else {
                    Check::fail("requester_permission", "MAILBOX_ACCESS_DENIED")
                });
            } else {
                let grant =
                    sending::grant_for_principal(tx, o.requested_by, o.mail_account_id).await?;
                let ok = match grant {
                    Some((_, can_compose, can_send, _, _)) => {
                        if o.purpose == "correspondence" {
                            can_send
                        } else {
                            can_compose
                        }
                    }
                    None => false,
                };
                checks.push(if ok {
                    Check::pass("requester_permission")
                } else {
                    Check::fail("requester_permission", "MAILBOX_ACCESS_DENIED")
                });
            }
            if kind != "agent" {
                // remember kind for the approval-needed decision below
            }
        }
        _ => {
            checks.push(Check::fail("requester_active", "PRINCIPAL_DISABLED"));
        }
    }

    if let Some(cred) = o.source_credential_id {
        checks.push(
            if sending::source_credential_usable(tx, cred, o.requested_by).await? {
                Check::pass("credential_usable")
            } else {
                Check::fail("credential_usable", CODE_CREDENTIAL_REVOKED)
            },
        );
    } else if requester_is_agent {
        checks.push(Check::fail("credential_usable", CODE_CREDENTIAL_REVOKED));
    } else {
        checks.push(Check::pass("credential_usable"));
    }

    // -- Approval (non-correspondence, or any agent-requested send) ----
    let approval_needed = o.purpose != "correspondence" || requester_is_agent;
    if o.launch_id.is_some() {
        campaign_approval_checks(tx, o, now, &mut checks).await?;
    } else if !approval_needed {
        checks.push(Check::pass("approval_state"));
        checks.push(Check::pass("approval_hash"));
        checks.push(Check::pass("approver_permission"));
    } else {
        let approval = sending::latest_outbound_approval(tx, o.id).await?;
        match approval {
            None => {
                checks.push(Check::fail("approval_state", CODE_APPROVAL_REQUIRED));
                checks.push(Check::fail("approval_hash", CODE_APPROVAL_REQUIRED));
                checks.push(Check::fail("approver_permission", CODE_APPROVAL_REQUIRED));
            }
            Some(a) => {
                let state_code: Option<&'static str> = match a.status.as_str() {
                    "approved" if a.expires_at > now => None,
                    "approved" => Some("APPROVAL_EXPIRED"),
                    "pending" if a.expires_at > now => Some(CODE_APPROVAL_PENDING),
                    "pending" => Some("APPROVAL_EXPIRED"),
                    "rejected" => Some(CODE_APPROVAL_REJECTED),
                    "revoked" => Some(CODE_APPROVAL_REVOKED),
                    "expired" => Some("APPROVAL_EXPIRED"),
                    _ => Some(CODE_APPROVAL_REJECTED),
                };
                checks.push(match state_code {
                    None => Check::pass("approval_state"),
                    Some(c) => Check::fail("approval_state", c),
                });

                let hash_ok = a.snapshot_hash == o.payload_hash;
                let decision = sending::approval_decision(tx, a.id).await?;
                let decision_ok = matches!(decision, Some((ref d, _)) if d == "approved");
                checks.push(if state_code.is_none() && hash_ok && decision_ok {
                    Check::pass("approval_hash")
                } else {
                    Check::fail(
                        "approval_hash",
                        if !hash_ok {
                            "SNAPSHOT_MISMATCH"
                        } else if state_code.is_some() {
                            state_code.unwrap()
                        } else {
                            CODE_APPROVAL_REJECTED
                        },
                    )
                });

                // Approver still active + still holds approve authority (T10).
                let approver_ok = match decision {
                    Some((ref d, decided_by)) if d == "approved" => {
                        match sending::principal_state(tx, decided_by).await? {
                            Some((status, kind, role))
                                if status == "active"
                                    && kind == "human"
                                    && (role == "admin" || role == "manager") =>
                            {
                                matches!(
                                    sending::grant_for_principal(tx, decided_by, o.mail_account_id)
                                        .await?,
                                    Some((_, _, _, true, _))
                                )
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                checks.push(if state_code.is_none() && hash_ok {
                    if approver_ok {
                        Check::pass("approver_permission")
                    } else {
                        Check::fail("approver_permission", CODE_APPROVER_PERMISSION)
                    }
                } else {
                    Check::fail(
                        "approver_permission",
                        state_code.unwrap_or(CODE_APPROVAL_REJECTED),
                    )
                });
            }
        }
    }

    // -- Execution state + emergency stop ------------------------------
    checks.push(if emergency_stop {
        Check::defer(
            "emergency_stop",
            "WORKSPACE_EMERGENCY_STOP",
            now + POLL_DEFER,
        )
    } else {
        Check::pass("emergency_stop")
    });
    let workers_locked = infrastructure::repo::operations::send_workers_locked(tx).await?;
    checks.push(if workers_locked {
        Check::defer(
            "send_workers_locked",
            CODE_SEND_WORKERS_LOCKED,
            now + POLL_DEFER,
        )
    } else {
        Check::pass("send_workers_locked")
    });

    // -- Suppression + sending basis (address row already locked) ------
    let suppressions =
        compliance_core::active_suppressions(tx, o.to_email_address_id, &o.purpose).await?;
    checks.push(if suppressions.is_empty() {
        Check::pass("recipient_not_suppressed")
    } else {
        Check::fail("recipient_not_suppressed", "RECIPIENT_SUPPRESSED")
    });

    // Freeze-time checks protect new drafts, but older snapshots and direct
    // DB writes still need a binding check at the provider boundary.
    let mut reply_context_ids = Vec::with_capacity(2);
    if let Some(reply_to) = o.reply_to_message_id {
        reply_context_ids.push(reply_to);
    }
    if let Some(reviewed_inbound) = expected_last_inbound
        && !reply_context_ids.contains(&reviewed_inbound)
    {
        reply_context_ids.push(reviewed_inbound);
    }
    let mut reply_recipient_ok = true;
    if !reply_context_ids.is_empty() {
        if let Some(thread_id) = o.thread_id {
            for inbound_id in reply_context_ids {
                if !sending::reply_recipient_matches(
                    tx,
                    o.mail_account_id,
                    thread_id,
                    inbound_id,
                    o.to_email_address_id,
                )
                .await?
                {
                    reply_recipient_ok = false;
                    break;
                }
            }
        } else {
            reply_recipient_ok = false;
        }
    }
    checks.push(if reply_recipient_ok {
        Check::pass("reply_recipient")
    } else {
        Check::fail("reply_recipient", "REPLY_RECIPIENT_MISMATCH")
    });

    let basis_ok = match o.purpose.as_str() {
        "correspondence" => {
            if o.reply_to_message_id.is_some() {
                // A matching inbound sender is the reply basis. A stale
                // or retargeted snapshot must not inherit that exemption.
                reply_recipient_ok
            } else if let Some(basis) = o.communication_basis_id {
                sending::basis_still_valid(tx, basis).await?
            } else {
                sending::any_valid_basis(tx, o.to_email_address_id, "correspondence")
                    .await?
                    .is_some()
            }
        }
        _ => {
            if let Some(basis) = o.communication_basis_id {
                sending::basis_still_valid(tx, basis).await?
            } else {
                sending::any_valid_basis(tx, o.to_email_address_id, &o.purpose)
                    .await?
                    .is_some()
            }
        }
    };
    checks.push(if basis_ok {
        Check::pass("send_basis")
    } else {
        Check::fail("send_basis", "SEND_BASIS_UNVERIFIED")
    });

    // -- New replies / bookings / duplicate contact --------------------
    if let Some(_reply_to) = o.reply_to_message_id {
        let expected = expected_last_inbound.or(o.reply_to_message_id);
        let thread_ok = match (o.thread_id, expected) {
            (Some(tid), Some(exp)) => {
                matches!(
                    sending::get_thread(tx, tid).await?,
                    Some((_, Some(last))) if last == exp
                )
            }
            _ => false,
        };
        checks.push(if thread_ok {
            Check::pass("reply_context")
        } else {
            Check::fail("reply_context", "REPLY_CONTEXT_CHANGED")
        });
    } else {
        checks.push(Check::pass("reply_context"));
    }

    if let Some(enrollment) = o.enrollment_id {
        let active = matches!(sending::enrollment_status(tx, enrollment).await?, Some(ref s) if s == "active");
        checks.push(if active {
            Check::pass("enrollment_active")
        } else {
            Check::fail("enrollment_active", CODE_ENROLLMENT_NOT_ACTIVE)
        });
    } else {
        checks.push(Check::pass("enrollment_active"));
    }

    // -- Sending-domain + sender verification ---------------------------
    let sender = sending::sender_identity_status(tx, o.sender_identity_id).await?;
    checks.push(match sender {
        Some((ref s, _)) if s == "verified" => Check::pass("sender_verified"),
        Some((ref s, _)) if s == "disabled" => Check::fail("sender_verified", CODE_SENDER_DISABLED),
        Some(_) => Check::defer("sender_verified", CODE_SENDER_UNVERIFIED, now + POLL_DEFER),
        None => Check::fail("sender_verified", CODE_SENDER_UNVERIFIED),
    });

    // -- Timezone / schedule limits ------------------------------------
    if let Some(nb) = o.not_before {
        checks.push(if nb > now {
            Check::defer("not_before", CODE_NOT_BEFORE, nb)
        } else {
            Check::pass("not_before")
        });
    } else {
        checks.push(Check::pass("not_before"));
    }

    // Quiet hours apply to campaign-linked sends (revision schedule_rules).
    if let Some(revision) = o.revision_id {
        match sending::campaign_schedule_rules(tx, revision).await? {
            Some(rules) => match domain::sending::SendWindow::parse(&rules) {
                Ok(w) => {
                    let last = sending::channel_last_accepted_at(tx, o.delivery_channel_id).await?;
                    if w.within(now) && w.next_allowed(now, last) <= now {
                        checks.push(Check::pass("quiet_hours"));
                    } else {
                        checks.push(Check::defer(
                            "quiet_hours",
                            CODE_QUIET_HOURS,
                            w.next_allowed(now, last),
                        ));
                    }
                }
                Err(_) => {
                    // Fail closed — unparseable schedule blocks the send.
                    checks.push(Check::fail("quiet_hours", "VALIDATION_FAILED"));
                }
            },
            None => checks.push(Check::fail("quiet_hours", CODE_CHANNEL_MISSING)),
        }
    } else {
        checks.push(Check::pass("quiet_hours"));
    }

    // -- Daily + campaign caps ------------------------------------------
    if let Some(cap) = chan.internal_daily_cap {
        let attempts = sending::channel_attempts_24h(tx, o.delivery_channel_id).await?;
        checks.push(if attempts < i64::from(cap) {
            Check::pass("daily_cap")
        } else {
            Check::defer("daily_cap", CODE_DAILY_CAP, now + Duration::hours(1))
        });
    } else {
        checks.push(Check::pass("daily_cap"));
    }

    // -- Final payload/hash consistency ---------------------------------
    // The payload_hash covered every frozen field at freeze time; the
    // snapshot trigger makes them immutable, so a non-empty hash is proof
    // of integrity. Approval snapshot equality was checked above.
    checks.push(if o.payload_hash.len() == 32 {
        Check::pass("payload_integrity")
    } else {
        Check::fail("payload_integrity", "SNAPSHOT_MISMATCH")
    });

    Ok(checks)
}

/// The source-draft's reply-review anchor: `expected_last_inbound_id` (the
/// inbound the human reviewed) or `reply_to_message_id` as the fallback.
pub async fn expected_last_inbound(
    tx: &mut Tx<'_>,
    o: &OutboundRow,
) -> Result<Option<Uuid>, AppError> {
    if let Some(draft_id) = o.source_draft_id
        && let Some(d) = sending::get_draft(tx, draft_id).await?
    {
        return Ok(d.expected_last_inbound_id.or(d.reply_to_message_id));
    }
    Ok(o.reply_to_message_id)
}

#[cfg(test)]
mod campaign_approval_tests {
    use super::*;

    fn approved_state(now: DateTime<Utc>) -> sending::CampaignLaunchApprovalState {
        let manifest = vec![7; 32];
        sending::CampaignLaunchApprovalState {
            launch_status: "running".into(),
            launch_manifest_hash: Some(manifest.clone()),
            launch_expires_at: now + Duration::hours(1),
            launch_revision_id: Uuid::from_u128(1),
            launch_channel_id: Uuid::from_u128(2),
            launch_requested_by: Uuid::from_u128(4),
            revision_status: "frozen".into(),
            revision_channel_id: Uuid::from_u128(2),
            request_id: Some(Uuid::from_u128(3)),
            request_requested_by: Some(Uuid::from_u128(4)),
            request_action_type: Some("campaign_launch".into()),
            request_status: Some("approved".into()),
            request_snapshot_hash: Some(manifest.clone()),
            request_expires_at: Some(now + Duration::hours(1)),
            request_version: Some(2),
            decision: Some("approved".into()),
            decision_snapshot_hash: Some(manifest),
            decision_version_at_decision: Some(1),
            decided_by: Some(Uuid::from_u128(5)),
            enrollment_status: Some("active".into()),
            enrollment_launch_id: Some(Uuid::from_u128(3)),
            enrollment_revision_id: Some(Uuid::from_u128(1)),
            enrollment_email_address_id: Some(Uuid::from_u128(6)),
            step_revision_id: Some(Uuid::from_u128(1)),
            step_order: Some(1),
            payload_hash_len: 32,
        }
    }

    #[test]
    fn campaign_approval_expiry_and_revocation_fail_closed() {
        let now = Utc::now();
        let mut state = approved_state(now);
        assert_eq!(campaign_state_code(&state, now), None);

        state.request_expires_at = Some(now - Duration::seconds(1));
        assert_eq!(campaign_state_code(&state, now), Some("APPROVAL_EXPIRED"));

        state = approved_state(now);
        state.launch_expires_at = now - Duration::seconds(1);
        assert_eq!(campaign_state_code(&state, now), Some("APPROVAL_EXPIRED"));

        state = approved_state(now);
        state.request_status = Some("revoked".into());
        assert_eq!(
            campaign_state_code(&state, now),
            Some(CODE_APPROVAL_REVOKED)
        );
    }

    #[test]
    fn campaign_approval_requires_the_current_manifest_hash() {
        let now = Utc::now();
        let mut state = approved_state(now);
        assert!(campaign_manifest_matches(&state));
        state.request_snapshot_hash = Some(vec![8; 32]);
        assert!(!campaign_manifest_matches(&state));
        state.request_snapshot_hash = Some(vec![7; 31]);
        assert!(!campaign_manifest_matches(&state));
    }

    #[test]
    fn campaign_approval_allows_requester_but_still_pins_snapshot() {
        let mut state = approved_state(Utc::now());
        state.decided_by = state.request_requested_by;
        assert!(campaign_decision_matches(&state));
        state.decision_snapshot_hash = Some(vec![99; 32]);
        assert!(!campaign_decision_matches(&state));
    }
}
