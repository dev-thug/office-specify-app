use serde_json::Value;

#[test]
fn catalog_exposes_operator_parity_tools_and_no_lead_proposal_tools() {
    let tools = mcp::all_tools();
    for name in [
        "crm_search_companies",
        "crm_search_contacts",
        "crm_create_company",
        "crm_update_company",
        "crm_create_contact",
        "templates_list",
        "templates_get",
        "templates_list_versions",
        "templates_get_version",
        "templates_render_version",
        "booking_list_meeting_types",
        "booking_get_meeting_type",
        "approvals_decide",
        "mail_freeze_draft",
        "mail_submit",
        "mail_submit_approved",
    ] {
        assert!(
            tools.iter().any(|tool| tool.name == name),
            "missing parity tool: {name}"
        );
    }
    // Lead proposals were replaced by direct CRM writes.
    for removed in [
        "research_propose_leads",
        "research_get_proposal",
        "research_accept_proposal",
        "crm_propose_update",
    ] {
        assert!(
            !tools.iter().any(|tool| tool.name == removed),
            "{removed} must not be exposed"
        );
    }
}

#[test]
fn initialization_explains_direct_crm_writes_scope_gating_and_kept_safeguards() {
    let response = mcp::jsonrpc::initialize_result();
    let instructions = response["instructions"].as_str().unwrap_or_default();
    for required in [
        // Direct CRM workflow.
        "crm_search_companies",
        "crm_create_company",
        "crm_update_company",
        "crm_create_contact",
        "expected_version",
        "VERSION_MISMATCH",
        "research_add_evidence",
        // Authority is gated by credential scopes.
        "scopes",
        "SCOPE_REQUIRED",
        "org.admin",
        "mail.approve",
        // Email workflow with approval/submission tools.
        "mail_freeze_draft",
        "approvals_decide",
        "mail_submit",
        "mail_submit_approved",
        // Kept safeguards.
        "agent drafts require approval",
        "mailbox grants",
        "Suppressions",
        "emergency stop",
        "idempotency_key",
        "untrusted",
    ] {
        assert!(
            instructions.contains(required),
            "missing guidance: {required}"
        );
    }
    for removed in [
        "research_propose_leads",
        "crm_propose_update",
        "research_get_proposal",
        "human review",
    ] {
        assert!(!instructions.contains(removed), "stale guidance: {removed}");
    }
}

#[test]
fn inbox_and_approved_send_fields_explain_their_actual_context() {
    let tools = mcp::all_tools();
    let field = |name: &str, key: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap()
            .input_schema["properties"][key]["description"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(field("mail_update_thread_status", "status").contains("open, pending or resolved"));
    assert!(field("mail_search_messages", "query").contains("plain-text bodies"));
    assert!(field("mail_create_reply_draft", "visibility").contains("mailbox (default)"));
    assert!(field("mail_submit_approved", "outbound_message_id").contains("frozen outbound"));
    assert!(field("mail_submit_approved", "expected_payload_hash").contains("binds the sender"));
}

#[test]
fn personalized_groups_explain_reuse_target_identity_and_approval_bound_batches() {
    let initialized = mcp::jsonrpc::initialize_result();
    let instructions = initialized["instructions"].as_str().unwrap();
    assert!(instructions.contains("sales_campaigns_create/add_target/create_message"));
    assert!(instructions.contains("sales_campaigns_freeze_message"));
    assert!(instructions.contains("sales_campaigns_queue"));
    assert!(
        instructions
            .to_ascii_lowercase()
            .contains("moving a card alone does not send")
    );
    let tools = mcp::all_tools();
    let queue = tools
        .iter()
        .find(|tool| tool.name == "sales_campaigns_queue")
        .unwrap();
    assert!(queue.description.contains("ten-minute"));
    let group_description = queue.input_schema["properties"]["campaign_id"]["description"]
        .as_str()
        .unwrap();
    assert!(group_description.contains("not a legacy bulk campaign"));
    assert!(
        queue.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "expected_target_version")
    );
    assert!(queue.external_effect);
    assert!(
        !tools
            .iter()
            .any(|tool| tool.name == "sales_campaigns_approve")
    );
}

#[test]
fn every_discoverable_input_explains_its_meaning() {
    for tool in mcp::all_tools() {
        for (field, schema) in tool.input_schema["properties"]
            .as_object()
            .into_iter()
            .flatten()
        {
            assert!(
                schema["description"]
                    .as_str()
                    .is_some_and(|text| text.len() > 20),
                "{}.{} lacks agent-facing instructions",
                tool.name,
                field
            );
        }
    }
}

#[test]
fn slot_range_guidance_matches_the_actual_date_format() {
    let tools = mcp::all_tools();
    let slots = tools
        .iter()
        .find(|tool| tool.name == "booking_list_slots")
        .unwrap();
    for field in ["from", "to"] {
        assert_eq!(slots.input_schema["properties"][field]["format"], "date");
        assert!(
            slots.input_schema["properties"][field]["description"]
                .as_str()
                .unwrap()
                .contains("YYYY-MM-DD")
        );
    }
}

#[test]
fn company_research_examples_only_use_registered_arguments() {
    let tools = mcp::all_tools();
    for name in [
        "crm_search_companies",
        "crm_create_company",
        "research_add_evidence",
        "crm_create_note",
    ] {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        let examples = tool.input_schema["examples"]
            .as_array()
            .expect("usage examples");
        assert!(!examples.is_empty());
        for example in examples {
            for field in example.as_object().unwrap().keys() {
                assert!(
                    tool.input_schema["properties"].get(field).is_some(),
                    "{name}.{field}"
                );
            }
            for required in tool.input_schema["required"]
                .as_array()
                .unwrap_or(&Vec::<Value>::new())
            {
                assert!(
                    example.get(required.as_str().unwrap()).is_some(),
                    "{name}: {required}"
                );
            }
        }
    }
}
