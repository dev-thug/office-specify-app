# Office-owned Zoho integration status

This document distinguishes implemented application paths from SDK-only methods.
Automated tests use synthetic local data and loopback Zoho responses. A separate
2026-10-10 account check verified OAuth, field metadata, list/contact/campaign
creation and one authorized Zoho **test email** received in Gmail with SPF, DKIM
and DMARC passing. The same test recipient subsequently confirmed an Office
unsubscribe link on the local test app: Office suppression and anonymous CRM
activity were recorded, the journaled Do-Not-Mail call completed, and Zoho's
contact detail showed **Unsubscribed**. This did not test delivery of a newly
mapped Office link in a real bulk campaign. Ordinary bulk `sendcampaign`, recipient merge substitution
and Forever Free entitlement were not verified by that test; the account was on
a trial. No private account identifiers or credentials are recorded here.
Official contracts are in [the capability matrix](zoho-api-capability-matrix.md).

## Primary architecture

Office owns templates and immutable versions through `/api/v1/email-templates`.
The Campaigns Free route uses a fixed, approved Office launch, one dedicated
provider list, one provider campaign and a signed public HTTPS `content_url`.
Zoho performs delivery. The separate prepaid Email API template store is not
needed and is not used by this workflow.

| Capability | Implementation |
|---|---|
| Office template CRUD, visual/source authoring, variables, immutable versions | Existing Office API/DB remains authoritative. The normal template screen does not invoke Zoho template CRUD. Basic HTML formatting/link policy remains; arbitrary images/CSS are not newly enabled. |
| Campaigns OAuth | Human-admin, session/workspace-bound, single-use authorization-code flow with encrypted tokens, refresh, disconnect and fixed US hosts. Native delivery requires Campaigns READ/CREATE/UPDATE plus contact READ/CREATE/UPDATE. Older read-only connections reconnect. |
| Account setup metadata | Mailing lists, recent campaigns, contact-field definitions and topics are exposed through authenticated Office APIs. `write_scopes_ready` describes stored requested scopes, not a live provider entitlement lookup. |
| Sender/channel setup | Admin records a Campaigns-verified sender with evidence; Office requires mailbox permissions. Channel review and enable are separate actions. Sender status is human-attested, not proof from an undocumented Zoho sender-verification endpoint. |
| Native export | One immutable export per frozen Office launch. It binds the source template/version, manifest, actual normalized recipient addresses, transformed HTML, field mapping and per-recipient booking values. Post-freeze variable inserts and recipient/address mismatches are rejected. |
| Contact sync | Dedicated private list, durable provider-operation journal, at most 25 contact mutations per sync request, resumable completed work, explicit opt-in and Office/provider suppression checks. Subscribe acceptance does not imply active/confirmed membership. |
| Approval | Existing independent human approval now binds both the Office manifest and the READY native export hash. The manager sees transformed HTML and mapping. An append-only export-review row is required by the final send gate. |
| Native bulk send | Final admin-session/SEND/hash confirmation, current sender/channel/approval/reviewer/mailbox permission checks, locked consent/suppression recheck, exact provider audience and mapped-value readback, one journaled `sendcampaign` call. No per-recipient dispatch jobs. |
| Signed HTML publishing | A scoped HMAC capability binds workspace, export, content hash and expiry; invalid/expired signatures are hidden as 404. Public HTML is served without OAuth tokens, with no-store/no-referrer and restrictive headers. Zoho must be able to fetch the public HTTPS origin. |
| Opt-out synchronization | New exports require a verified recipient-specific field for the primary Office unsubscribe link, alongside the mandatory native Zoho footer. Office commits suppression before journaled Do-Not-Mail propagation; admin-only readback exposes pending/completed/unknown outcomes. New connections queue existing blocks for bounded reconciliation. Background polling reads the full bounded, paginated dedicated unsubscribe list back into Office. Supplemental recipient reports remain partial. |
| Local privacy erasure | Frozen native variables and affected export content are scrubbed with the existing audited maintenance role; unrelated shared-address data is preserved. In-flight/ambiguous provider writes block erasure until reconciled. This does not delete provider data or recall a sent campaign. |
| Reports and unknown outcomes | Provider aggregate/status reads and partial recipient evidence are separate from delivery/read claims. Unknown creates/sends are never blindly retried; readback can reconcile only when provider evidence identifies the result. |
| Zoho Mail | Separate configured OAuth correspondence adapter for account discovery, Inbox polling, body/details and provider-ID replies. Marketing is not routed through Mail. Unknown Mail sends have no reliable correlation lookup. |

## What is still outside the application flow

- Zoho Mail search, remote Mail drafts/templates and incoming attachment methods remain SDK-only; outgoing attachment upload is not implemented.
- Campaigns custom-field creation is SDK-only. The application reads field definitions/topics; native merge-tag listing/creation is not invented from inconsistent documentation. Use verified account mappings.
- Campaigns scheduling, workflows, follow-ups and webhooks are not part of this Free route.
- Prepaid Email API v2 remains a separately configured optional backend. Its template endpoints still exist for compatibility, but the Office template screen no longer mounts its library. Its sending/status/suppression SDK methods are not used by the native Free workflow.
- The generic marketing `DeliveryProvider` stays disabled outside explicit fixtures. Native Campaigns delivery uses its separate bulk export API, not that per-message adapter.

## Practical limits

Campaigns detail readback does not return saved HTML or an atomic compare-and-swap
version. Office serializes its own account writes and verifies metadata, exact
membership and mapped field values, but external Zoho edits can race those checks.
Review the provider preview and keep staged campaign/list edits in this workflow.

The official contact JSON examples are inconsistent. Standard JSON writes were
accepted by the tested account. Field metadata accepts the observed nested
`response.fieldnames.fieldname` response as well as the documented shape.
Recipient-report pagination is undocumented, so those reports are labeled partial
and are not the sole opt-out source. The API does not establish a complete
organization-wide Do-Not-Mail registry read; an ambiguous suppression push stays
unknown rather than being represented as confirmed or automatically repeated.

Office booking and unsubscribe endpoints remain local. Campaigns still requires
its native unsubscribe link. A submitted bulk campaign cannot be recalled by
Office's ordinary local pause/cancel actions. Provider acceptance is not delivery,
reading or a guarantee of quota availability.

See [setup and recovery](../runbooks/zoho-campaigns.md) for server configuration and
the exact workflow. Automated development checks do not send live mail; the
separate authorized account test used Zoho's test-email action only.
