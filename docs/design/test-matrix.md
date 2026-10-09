# Test matrix — T01–T36 (source §18.1)

Each row maps to named tests in the owning crate. `cargo nextest run
--workspace` with `DATABASE_URL=postgres://…/outreach_test` runs everything
marked **implemented**.

| T | Requirement (§18.1) | Owner | Test location | Status |
|---|---|---|---|---|
| T01 | Cross-workspace id → 404/403, zero rows | foundation | `infrastructure/tests/foundation.rs::t01_cross_workspace_isolation` | ✅ |
| T02 | Admin without mailbox grant sees nothing | mailboxes (Task 3) | `api/tests/` module tests | pending |
| T03 | Agent approve/grant/submit → deny + audit | org/agents (Task 3/12) | — | pending |
| T04 | 20× same idempotency key + same request → 1 exec | foundation | `foundation.rs::t04_t05_idempotency_concurrency_and_reuse` | ✅ |
| T05 | Same key, different body → 409 | foundation | same test | ✅ |
| T06 | Step/enrollment on different revision → FK fail | campaigns (Task 7) | — | pending |
| T07 | Thread/message mailbox mismatch → FK fail | inbox (Task 9) | — | pending |
| T08 | Post-approval snapshot/target change → unusable | approvals (Task 8) | — | pending |
| T09 | Self-approval → reject | approvals (Task 8) | — | pending (DB trigger exists: `reject_self_approval`) |
| T10 | Approver/requester perms revoked before dispatch | sending (Task 9) | — | pending |
| T11 | Suppression commit then new send permit → deny | compliance (Task 10) | — | pending |
| T12 | Suppression during in-flight submit → record, no new submits | sending (Task 9) | — | pending |
| T13 | Worker crash after provider accept → unknown → reconcile | sending (Task 9) | — | pending |
| T14 | Webhook dup/out-of-order/replay → no dupes | inbox (Task 9) | — | pending |
| T15 | Two users submit same-thread reply → 1 claim | sending (Task 9) | — | pending |
| T16 | New inbound during draft → 409 re-review | sending (Task 9) | — | pending |
| T17 | Per-principal read state isolation | inbox (Task 9) | — | pending |
| T18 | 50 concurrent overlapping holds → 1 wins | booking (Task 11) | — | pending (EXCLUDE constraint exists) |
| T19 | Buffer overlap blocked | booking (Task 11) | — | pending |
| T20 | DST duplicate/nonexistent local time | booking (Task 11) | — | pending |
| T21 | Calendar-create timeout → unknown, slot not auto-released | booking (Task 11) | — | pending |
| T22 | Hold expiry → expired; confirming not auto-released | booking (Task 11) | — | pending |
| T23 | Unsubscribe GET/HEAD/prefetch → no state change | compliance (Task 10) | — | pending |
| T24 | Valid token repeated POST → same success, no scope creep | compliance (Task 10) | — | pending |
| T25 | RFC one-click POST without cookies → works | compliance (Task 10) | — | pending |
| T26 | Inbound HTML script/form/remote image blocked | inbox (Task 9) | — | pending |
| T27 | Private IP/redirect/DNS-rebinding research URL → blocked | research (Task 5) | — | pending |
| T28 | Inbound-mail prompt injection → no tool/scope change | agents (Task 12) | — | pending |
| T29 | Redis init/outage → limits fail-safe, no lost jobs | operations (Task 11) | — | pending (fail-mode enum exists) |
| T30 | Old-DB restore → send workers stay locked pending reconcile | operations (Task 11) | — | pending |
| T31 | One address on N contacts → 1 enrollment per launch | crm (Task 4) | — | pending |
| T32 | Suppression survives contact delete/re-import | compliance (Task 10) | — | pending |
| T33 | marketing + Zoho channel activation → blocked | mailboxes (Task 3) | — | pending |
| T34 | accepted-but-undelivered → no delivered/read UI claim | sending (Task 9) | — | pending |
| T35 | Agent unsubstantiated email candidate → storable, unsendable | research (Task 5) | — | pending |
| T36 | Retention delete → nothing left in search/attachments/cache | operations (Task 11) | — | pending |

## Foundation-level tests beyond the T rows (Task 1 step 9)

| Requirement | Test |
|---|---|
| Migrations apply on empty DB | `migrations_applied_cleanly` + `sqlx migrate run` verified on fresh `outreach_test`/`outreach_dev` |
| RLS context never leaks across pooled connections | inside `t01_cross_workspace_isolation` |
| Lease fencing (stale token can't complete) | `job_lease_fencing_and_reclaim` |
| Lease expiry reclaim + retry/dead transitions | same test |
| Append-only audit guard | `append_only_guard_blocks_audit_mutation` |
| Login lockout after 5 failures | `api/tests/auth_flows.rs::login_lockout_after_five_failures` |
| CSRF rejection on session mutations | `session_mutation_without_csrf_is_rejected` |
| `/me` unauthenticated → 401 problem+json | `me_requires_auth` |
| Login sets session+CSRF cookies | `login_sets_cookies_and_me_returns_principal` |
