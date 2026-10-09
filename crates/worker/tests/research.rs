//! Task 5 (research) worker-side / data-level tests.
//!
//! - `t35_*`: an accepted proposal's email candidate is stored (verbatim,
//!   never fabricated) but carries only a `pending` communication basis —
//!   the §8.4 send-eligibility predicate (`status='valid'` basis required)
//!   can never match at the data level.
//! - Accept links/dedupes by normalized email + emits `lead.accepted`.
//! - Agents may propose but never decide/review.

mod common;

use contracts::Scope;
use contracts::ScopeSet;
use contracts::research::{
    CreateLeadProposalsRequest, DecisionKind, EvidenceReviewDecision, EvidenceReviewRequest,
    LeadProposalDecisionRequest,
};
use infrastructure::testing::{ctx_for, ctx_for_credential, seed_workspace};
use serde_json::json;
use uuid::Uuid;

fn proposal(company: &str, email: Option<&str>) -> CreateLeadProposalsRequest {
    let contacts = match email {
        Some(e) => json!([{"name": "Kim Research", "email": e}]),
        None => json!([]),
    };
    serde_json::from_value(json!({
        "proposals": [{
            "external_key": format!("ext-{company}"),
            "company_name": company,
            "domain": format!("{company}.example"),
            "proposed_data": {"industry": "mfg", "contacts": contacts},
            "evidence": [{
                "field_path": "industry",
                "source_url": "https://public.example/source",
                "observed_at": "2026-01-01T00:00:00Z",
                "value": "mfg",
                "evidence_type": "observed"
            }]
        }]
    }))
    .unwrap()
}

#[tokio::test]
async fn t35_accepted_candidate_email_stored_with_pending_basis_unsendable() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let ws = world.workspace_id;

    // Agent proposes (service-credential ctx, research.propose scope).
    let agent_ctx = ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        ScopeSet::of([Scope::ResearchPropose]),
        "t35-propose",
    );
    let res = application::research::create_proposals(
        &db,
        &agent_ctx,
        &proposal("acme", Some("kim@acme.example")),
    )
    .await
    .unwrap();
    let proposal_id = res.proposals[0].id;

    // Human manager accepts (research.write).
    let mgr = ctx_for(&world, &world.manager, "t35-accept");
    let out = application::research::decide(
        &db,
        &mgr,
        proposal_id,
        1,
        &LeadProposalDecisionRequest {
            decision: DecisionKind::Accept,
            company_id: None,
            reason: Some("looks real".into()),
        },
    )
    .await
    .unwrap();
    let address_id = out.email_address_ids[0];
    let contact_id = out.contact_ids[0];

    let mut tx = db.begin(ws).await.unwrap();

    // Address stored verbatim, deduped on the normalized key.
    let (orig, key): (String, String) = sqlx::query_as(
        "SELECT address_original, normalized_key FROM email_addresses
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(key, "kim@acme.example");
    assert_eq!(orig, "kim@acme.example");

    // Contact↔address link exists.
    let linked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3",
    )
    .bind(ws)
    .bind(contact_id)
    .bind(address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(linked, 1);

    // The ONLY basis is 'pending' — the eligibility predicate needs
    // status='valid' + reviewer fields; a pending basis can never satisfy
    // it at the data level (T35 core assertion).
    let bases: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT status, reviewed_by FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2",
    )
    .bind(ws)
    .bind(address_id)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert_eq!(bases.len(), 1, "expected one pending basis: {bases:?}");
    assert_eq!(bases[0].0, "pending");
    assert!(bases[0].1.is_none());

    // The §8.4 eligibility query shape — valid + reviewed + unrevoked.
    let eligible: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2
            AND status = 'valid' AND reviewed_by IS NOT NULL
            AND revoked_at IS NULL
            AND (valid_until IS NULL OR valid_until > now())",
    )
    .bind(ws)
    .bind(address_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        eligible, 0,
        "pending basis must never satisfy send eligibility"
    );

    // lead.accepted outbox carries ids only (B.1 payloads rule).
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM outbox_events
          WHERE workspace_id = $1 AND event_type = 'lead.accepted'
            AND aggregate_id = $2",
    )
    .bind(ws)
    .bind(proposal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        payload["lead_proposal_id"].as_str().unwrap(),
        proposal_id.to_string()
    );
    assert!(payload["company_id"].is_string());
    assert_eq!(
        payload["contact_id"].as_str().unwrap(),
        contact_id.to_string()
    );
    assert!(payload.get("email").is_none() && payload.get("proposed_data").is_none());

    let proposed: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM outbox_events
          WHERE workspace_id = $1 AND event_type = 'lead.proposed'
            AND aggregate_id = $2",
    )
    .bind(ws)
    .bind(proposal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(proposed, json!({"lead_proposal_id": proposal_id}));

    let evidence_event: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM outbox_events
          WHERE workspace_id = $1 AND event_type = 'research.evidence_created'
            AND payload ->> 'lead_proposal_id' = $2
          LIMIT 1",
    )
    .bind(ws)
    .bind(proposal_id.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert!(evidence_event["research_evidence_id"].is_string());
    assert_eq!(
        evidence_event["lead_proposal_id"].as_str().unwrap(),
        proposal_id.to_string()
    );
    assert!(evidence_event.get("source_url").is_none());
    assert!(evidence_event.get("value").is_none());
    tx.rollback().await.unwrap();

    // Agents may never decide (§13.4) — 403 at the authz layer.
    let err = application::research::decide(
        &db,
        &agent_ctx,
        proposal_id,
        2,
        &LeadProposalDecisionRequest {
            decision: DecisionKind::Reject,
            company_id: None,
            reason: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 403, "agent decision must be forbidden");

    // Nor review evidence.
    let ev_id: Uuid = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = sqlx::query_scalar(
            "SELECT id FROM research_evidence
              WHERE workspace_id = $1 AND lead_proposal_id = $2 LIMIT 1",
        )
        .bind(ws)
        .bind(proposal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.rollback().await.unwrap();
        id
    };
    let err = application::research::review_evidence(
        &db,
        &agent_ctx,
        ev_id,
        &EvidenceReviewRequest {
            decision: EvidenceReviewDecision::Verified,
            reason: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 403);
}

#[tokio::test]
async fn accept_dedupes_company_and_address_by_normalized_values() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let ws = world.workspace_id;
    let mgr = ctx_for(&world, &world.manager, "dedupe");

    // Pre-existing company with the same normalized domain (different
    // case/trailing-dot input).
    let mut tx = db.begin(ws).await.unwrap();
    let existing_company = infrastructure::repo::research::insert_company(
        &mut tx,
        "Acme",
        Some("acme.example"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let res = application::research::create_proposals(
        &db,
        &mgr,
        &serde_json::from_value(json!({
            "proposals": [{
                "company_name": "ACME Inc",
                "domain": "ACME.example.",
                "proposed_data": {"contacts": [{"name": "A", "email": "A@ACME.EXAMPLE"}]},
                "evidence": [{
                    "field_path": "f", "source_url": "https://ok.example",
                    "observed_at": "2026-01-01T00:00:00Z", "value": 1,
                    "evidence_type": "inferred"
                }]
            }]
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let pid = res.proposals[0].id;
    // Creation flags the same-domain duplicate (reported, never merged).
    assert_eq!(
        res.proposals[0].duplicate_company_ids,
        vec![existing_company]
    );

    let out = application::research::decide(
        &db,
        &mgr,
        pid,
        1,
        &LeadProposalDecisionRequest {
            decision: DecisionKind::Accept,
            company_id: None,
            reason: None,
        },
    )
    .await
    .unwrap();
    // Accept LINKS to the existing company by normalized domain — no new
    // company row.
    assert_eq!(out.accepted_company_id, Some(existing_company));

    let mut tx = db.begin(ws).await.unwrap();
    let n_companies: i64 =
        sqlx::query_scalar("SELECT count(*) FROM companies WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(
        n_companies, 1,
        "domain dedupe must not create a second company"
    );
    let key: String =
        sqlx::query_scalar("SELECT normalized_key FROM email_addresses WHERE workspace_id = $1")
            .bind(ws)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(key, "a@acme.example", "normalized dedupe key lowercases");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn crm_update_proposal_stays_pending_until_human_acceptance() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let company_id = infrastructure::repo::research::insert_company(
        &mut tx,
        "Acme",
        Some("acme.example"),
        Some("manufacturing"),
        None,
        None,
    )
    .await
    .unwrap();
    let run_id = infrastructure::repo::agents::insert_run(
        &mut tx,
        world.agent.principal_id,
        world.admin.principal_id,
        None,
        "research company updates",
        &json!({"max_candidates": 5}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let ctx = ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        ScopeSet::of([Scope::ResearchPropose]),
        "crm-propose-update",
    );
    let req: contracts::research::ProposeUpdateRequest = serde_json::from_value(json!({
        "idempotency_key": "crm-update-company-001",
        "agent_run_id": run_id,
        "target": {"kind": "company", "id": company_id},
        "expected_version": 1,
        "field_changes": {"industry": "industrial automation"},
        "evidence": [{
            "field_path": "industry",
            "source_url": "https://acme.example/about",
            "observed_at": "2026-10-09T00:00:00Z",
            "value": "industrial automation",
            "evidence_type": "observed"
        }]
    }))
    .unwrap();

    let first = application::research::propose_update(&db, &ctx, &req)
        .await
        .unwrap();
    assert_eq!(
        first.status,
        contracts::research::LeadProposalStatus::Pending
    );
    assert_eq!(first.proposed_data["proposal_kind"], "crm_update");
    assert_eq!(first.proposed_data["target"]["id"], company_id.to_string());
    assert_eq!(first.proposed_data["expected_version"], 1);
    assert_eq!(
        first.proposed_data["field_changes"]["industry"]["before"],
        "manufacturing"
    );
    assert_eq!(
        first.proposed_data["field_changes"]["industry"]["after"],
        "industrial automation"
    );

    let mut tx = db.begin(ws).await.unwrap();
    let industry: String =
        sqlx::query_scalar("SELECT industry FROM companies WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(company_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(industry, "manufacturing", "proposal must not mutate CRM");
    let attached: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM research_evidence
          WHERE workspace_id = $1 AND lead_proposal_id = $2 AND review_status = 'pending'",
    )
    .bind(ws)
    .bind(first.id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(attached, 1);
    tx.rollback().await.unwrap();

    // A human decision is the only step that applies the proposal. The target
    // version is checked again at acceptance and the CRM row changes then.
    let manager = ctx_for(&world, &world.manager, "crm-update-accept");
    let accepted = application::research::decide(
        &db,
        &manager,
        first.id,
        first.version,
        &LeadProposalDecisionRequest {
            decision: DecisionKind::Accept,
            company_id: None,
            reason: Some("confirmed from the source".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        accepted.status,
        contracts::research::LeadProposalStatus::Accepted
    );
    assert_eq!(accepted.accepted_company_id, Some(company_id));
    let mut tx = db.begin(ws).await.unwrap();
    let industry: String =
        sqlx::query_scalar("SELECT industry FROM companies WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(company_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(industry, "industrial automation");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn crm_update_proposal_idempotency_binds_the_key_to_the_diff() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let company_id = infrastructure::repo::research::insert_company(
        &mut tx,
        "Beta",
        Some("beta.example"),
        Some("software"),
        None,
        None,
    )
    .await
    .unwrap();
    let run_id = infrastructure::repo::agents::insert_run(
        &mut tx,
        world.agent.principal_id,
        world.admin.principal_id,
        None,
        "research company updates",
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let ctx = ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        ScopeSet::of([Scope::ResearchPropose]),
        "crm-propose-update-idem",
    );
    let make_req = |industry: &str| {
        serde_json::from_value::<contracts::research::ProposeUpdateRequest>(json!({
            "idempotency_key": "crm-update-company-002",
            "agent_run_id": run_id,
            "target": {"kind": "company", "id": company_id},
            "expected_version": 1,
            "field_changes": {"industry": industry},
            "evidence": [{
                "field_path": "industry",
                "source_url": "https://beta.example/about",
                "observed_at": "2026-10-09T00:00:00Z",
                "value": industry,
                "evidence_type": "observed"
            }]
        }))
        .unwrap()
    };

    let first = application::research::propose_update(&db, &ctx, &make_req("AI"))
        .await
        .unwrap();
    let replay = application::research::propose_update(&db, &ctx, &make_req("AI"))
        .await
        .unwrap();
    assert_eq!(replay.id, first.id);

    let reused = application::research::propose_update(&db, &ctx, &make_req("fintech"))
        .await
        .unwrap_err();
    assert_eq!(reused.code, contracts::ErrorCode::IdempotencyKeyReused);
}
