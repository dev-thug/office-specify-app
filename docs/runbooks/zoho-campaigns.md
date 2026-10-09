# Office templates → Zoho Campaigns Free

Office owns template creation, visual/HTML editing, variable schemas and immutable
versions through its existing `/api/v1/email-templates` APIs. The free Campaigns
path does not use Zoho's separately prepaid template-library or Email API product.
Office publishes a signed immutable HTML URL and Campaigns imports it through
`createCampaign.content_url`; Zoho performs the actual bulk delivery.

## Connect and configure

1. Register a **Server-based Application** in the US [Zoho API Console](https://api-console.zoho.com/). Register the exact callback shown in Settings → Mail accounts → Zoho Campaigns.
2. Set the private server environment `ZOHO_CAMPAIGNS_CLIENT_ID`, `ZOHO_CAMPAIGNS_CLIENT_SECRET`, and optionally `ZOHO_CAMPAIGNS_REDIRECT_URI`. The binaries do not automatically load `.env`. Restart the API and worker with the environment loaded. Production requires the encryption/HMAC keys and Redis described in `.env.example`.
3. Connect as a human workspace administrator. Existing read-only connections must reconnect: the native path requests Campaigns READ/CREATE/UPDATE and contacts READ/CREATE/UPDATE, with no ALL/DELETE scopes. The settings flag records whether these scopes were requested; provider authorization remains authoritative.
4. Select the Office mailbox used for ownership/ACL. Register an address **already verified in the intended Campaigns organization**, record the verification evidence, and select the sender identity. This records a human verification assertion; it does not send a verification message or claim an undocumented Zoho sender-status API check.
5. Create a Campaigns **marketing** delivery channel. Review the provider policy with recorded evidence, then enable it using its current version. A paid Zoho Mail plan is not required for this sending API and does not upgrade Campaigns. Office mailbox permissions still govern who can compose, approve and send.
6. Configure a public HTTPS application origin. Localhost can be used for development OAuth where accepted by Zoho, but Zoho cannot fetch a localhost campaign HTML URL. Native campaign sync therefore rejects a local/private content origin.

Never put secrets, OAuth callback codes, tokens or API keys in the browser,
Git, issue bodies or verification evidence. Tokens remain in the encrypted
server SecretStore. There is no paid signup or credit purchase in this flow.

## Author, approve and send

1. Create the template and immutable version in **Office → Templates**. The basic editor supports text formatting and links; its existing HTML policy still applies.
2. Create a campaign selecting the enabled Campaigns marketing channel and that Office version. Campaigns Free uses one immediate step. Scheduling, weekday/time windows, delays and follow-up automation are excluded for this route; an Office meeting type binding is retained where needed.
3. Choose contacts with recorded **explicit marketing consent**. Office suppression and invalid/expired consent block the export. A reviewed legal exception alone does not establish Zoho opt-in.
4. Freeze the launch. Office fixes the recipient list, exact template version, resolved variable values and recipient-bound booking links. The existing human approval chain remains in force; approving a native launch never queues individual message dispatches.
5. Prepare the native export in the launch's **Office template / Zoho delivery** panel. Choose the account topic, where required, from the provider metadata. Values that differ across recipients need an explicit mapping to a Campaigns field and native merge tag. Fields/topics are read from Campaigns; the documented API cannot list native merge tags, so verify those tags in the account rather than guessing them.
6. Sync contacts. Each request stages at most 25 contact operations and records completed results durably. Continue syncing while the export is `syncing`; confirmed active list membership is checked independently of subscribe acceptance. Pending double opt-in keeps the export unready.
7. Office creates one dedicated provider list and one native campaign with the signed `content_url`. Review the exact audience counts, Office export HTML, mapping, sender/topic and provider metadata. The export contains one Campaigns-native organization opt-out tag. Office's own unsubscribe URL is not substituted for Zoho's required mechanism.
8. Obtain the existing Office launch approval from another authorized human. Review the provider campaign in Zoho where required, then use the Office final send confirmation (`SEND`) with the current export hash. The backend rechecks the approval, current session/permissions, sender/channel, consent, suppression, exact provider audience and mapped values before the single bulk send. If more than 100 historical lists need inspection, no send is submitted: repeat the explicit confirmation to continue the same scan within five minutes. A read-only refresh or background poll preserves that checkpoint; an expired scan starts fresh.

`in_progress` means Zoho accepted the campaign for processing; it does not mean
all recipients received or read it. The UI does not synthesize recipient delivery
or engagement evidence. A submitted Campaigns campaign cannot be recalled by
Office's ordinary pause/cancel actions; those actions prevent future local work.

## Unsubscribe and reports

Office unsubscribe and booking routes remain local. An Office marketing/all
suppression is committed first, stops local enrollment, and queues a separately
journaled Campaigns Do-Not-Mail propagation. Native Campaigns opt-outs are polled
back to Office. Background polling is data synchronization, not a marketing
workflow or scheduled send.

The dedicated list's fully paginated unsubscribe status is the primary opt-out
source. Recipient-report pages are supplemental: their pagination contract is not
documented, so coverage is marked partial. Explicit `senthardbounce` records can
establish hard-bounce evidence; generic bounced-list status must not be relabeled
as a hard bounce without such evidence. Aggregates are provider counts, not proof
of an individual contact's delivery/open/click.

## Unknown outcomes and external edits

Every provider mutation is journaled before its request. Timeout, reset, ambiguous
response or lost worker result cannot cause an automatic create/send retry.
Refresh/reconciliation uses provider reads only. If provider evidence cannot prove
an unknown result, keep it unknown for operator review; do not use a new key to
blindly create or send again.

Office serializes its own writes per Campaigns account and verifies metadata,
exact list membership and mapped values before submission. The Campaigns details
API does not return saved HTML and has no atomic compare-and-swap. External edits
in Zoho can therefore race Office checks. Keep the staged native campaign/list
under this workflow's control and review Zoho's preview before final confirmation.

The official contact-write examples contain inconsistent JSON notation. The
client sends standard JSON as documented, with loopback tests; actual account
acceptance, merge rendering, footer behavior, sender/domain verification and
quotas still require a configured account check. Local mock tests are not live
Zoho delivery evidence.

Custom contact fields use Zoho’s documented `$[UD:FIELD_NAME]$` merge-tag syntax. Confirm the exact tag in the Campaigns account; Office accepts bare standard tags and `UD:` custom tags without fallback values, and reserves `LI:` for the mandatory native unsubscribe link. See [Zoho personalization documentation](https://help.zoho.com/portal/en/kb/campaigns/user-guide/email-campaigns/create-campaign/articles/how-to-personalize-your-email). Native merge tags must be verified with an actual test-list campaign: Zoho’s test-email preview does not substitute them.

## Privacy maintenance

Office contact erasure also clears frozen native recipient variables, booking links
and the affected export content. It cancels local export processing and records
that provider cancellation/deletion was not requested. A live or ambiguous provider
mutation must be reconciled before erasure can commit; an operator must handle any
remote data removal separately. Completed provider campaigns cannot be recalled by
redacting their local snapshots.
