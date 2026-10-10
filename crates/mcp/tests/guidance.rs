use serde_json::Value;

#[test]
fn discovery_tools_complete_the_research_prerequisites_without_approval_actions() {
    let tools = mcp::all_tools();
    for name in [
        "crm_search_contacts",
        "templates_list",
        "templates_get",
        "templates_list_versions",
        "templates_get_version",
        "templates_render_version",
        "booking_list_meeting_types",
        "booking_get_meeting_type",
        "research_get_proposal",
    ] {
        assert!(
            tools.iter().any(|tool| tool.name == name),
            "missing discovery tool: {name}"
        );
    }
    for forbidden in [
        "research_accept_proposal",
        "approvals_decide",
        "mail_submit",
        "mailbox_grant",
    ] {
        assert!(!tools.iter().any(|tool| tool.name == forbidden));
    }
}

#[test]
fn initialization_explains_company_research_storage_and_human_handoffs() {
    let response = mcp::jsonrpc::initialize_result();
    let instructions = response["instructions"].as_str().unwrap_or_default();
    for required in [
        "crm_search_companies",
        "research_propose_leads",
        "crm_propose_update",
        "proposed_data.contacts",
        "expected_version",
        "idempotency_key",
        "pending",
        "human",
        "mail.send",
    ] {
        assert!(
            instructions.contains(required),
            "missing guidance: {required}"
        );
    }
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
        "research_propose_leads",
        "crm_propose_update",
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

#[test]
fn lead_example_uses_fields_consumed_by_the_acceptance_path() {
    let tools = mcp::all_tools();
    let research = tools
        .iter()
        .find(|tool| tool.name == "research_propose_leads")
        .unwrap();
    let example = &research.input_schema["examples"][0]["proposals"][0];
    let lead: contracts::research::LeadProposalInput =
        serde_json::from_value(example.clone()).unwrap();
    assert!(domain::research::validate_proposal(&lead, 0).is_empty());
    let company = domain::research::extract_company_fields(&lead.proposed_data);
    assert_eq!(company.industry.as_deref(), Some("Software"));
    assert_eq!(company.country_code.as_deref(), Some("KR"));
    assert_eq!(company.website_url.as_deref(), Some("https://example.com"));
    let contacts = domain::research::extract_contacts(&lead.proposed_data);
    assert_eq!(contacts[0].email.as_deref(), Some("sales@example.com"));
    assert!(
        lead.evidence
            .iter()
            .any(|evidence| evidence.field_path == "contacts[0].email"
                && evidence.value == "sales@example.com")
    );
    let update = tools
        .iter()
        .find(|tool| tool.name == "crm_propose_update")
        .unwrap();
    let _: contracts::research::ProposeUpdateRequest =
        serde_json::from_value(update.input_schema["examples"][0].clone()).unwrap();
}
