# Office

Office is an open-source, self-hosted workspace for contacts, companies, email
templates and campaigns, meeting bookings, approvals, and operational controls.
The backend is a Rust workspace; the web interface is a Next.js app in
[`apps/web`](apps/web/).

## Start here

- [Local development](docs/runbooks/local-development.md) — prerequisites,
  database setup, migrations, first admin, and service startup.
- [Upgrade from retired Zoho Campaigns](docs/runbooks/local-development.md#upgrade-from-the-retired-zoho-campaigns-integration) — stop old services,
  clean obsolete provider secrets, migrate, and restart.
- [SMTP setup](docs/runbooks/smtp.md) — configure Office-owned campaigns to send
  through an authenticated SMTP relay, with optional opt-in IMAP inbox sync.
- [MCP agent guide](docs/runbooks/mcp-agent-guide.md) — scoped agent access,
  template authoring, inbox management, and separately approved submission.
- [Zoho integration status](docs/design/zoho-integration-status.md) — current
  Zoho Mail and SMTP behavior, plus the retired Campaigns integration record.
- [Local booking setup](docs/runbooks/local-booking.md) — set up the internal
  calendar, availability, meeting types, and private booking links.
- [Unknown-send reconciliation](docs/runbooks/unknown-send-reconciliation.md)
  and [database restore](docs/runbooks/restore.md) — operator recovery guides.
- [Source design](docs/design/source/outreach-os-design-v1.0.md),
  [API and MCP contracts](docs/design/integration-contracts.md), and the
  [acceptance matrix](docs/design/test-matrix.md) are historical design and
  test references; consult the current integration status for supported routes.

## Features

For agent integration, read the [MCP connection, company-research field mapping,
and feature coverage guide](docs/runbooks/mcp-agent-guide.md). MCP exposes a
scoped subset of read, research, template-authoring, inbox, and approved-submit
workflows; it does **not** expose every UI/REST action. Its initialization
instructions and input schemas explain record IDs, field destinations, examples,
and human handoffs. The live `tools/list` response is authoritative.

New to Office? Open **Usage guide** in the top bar for the setup-to-send flow
and instructions for every menu. Hover or focus the question-mark icon beside
a menu or the current screen name for a short explanation; click it (or press
Enter) for when to use the feature and numbered steps. On touch screens, tap
the same icon. Escape closes the instructions and returns focus to the button.

| Area | What Office provides |
| --- | --- |
| CRM | Contact and company records, search, notes, activity, and CSV imports. |
| Email content | One rendered email canvas with a toggle to HTML source, variable presets and schema, recipient preview, and reusable immutable template versions. |
| Office campaigns | Company-specific draft kanban with a Settings-managed default sender, recipient preview, independent human review, and explicit ten-minute batch queueing. Existing common-template bulk campaigns remain separately available. Consent, suppression and SMTP delivery checks apply to both workflows. |
| Inbox | Received mail imported by Zoho Mail OAuth or all received `INBOX` mail through an explicitly enabled SMTP-profile IMAP monitor; local thread status and read state remain separate from provider flags. |
| Meetings | Internal meeting types, host availability, booking links, and persisted booking, reschedule, and cancellation records. The local calendar does not use OAuth. |
| Access and operations | Admin, Manager, Member, Agent, and System principals; mailbox grants; approvals; emergency stop; job and delivery status; audit records; and recovery runbooks. |
| MCP | A Streamable HTTP endpoint with workspace-scoped service credentials and policy-checked tools for CRM research, template authoring, drafts, inbox management, and approved-message/campaign submission. Agents cannot decide approvals or use general mail-send or approval scopes. |

Office keeps an explicit separation between preparing an action and carrying it
out. An agent-created sales message stays a draft until a human approves its
exact snapshot. Approval records the decision but does not queue delivery; a
later explicit `mail_submit_approved` call with the narrow
`mail.submit_approved` scope can submit only that still-current approved
snapshot. Campaign approval also requires a separate explicit
`campaign_submit_approved` action with `campaign.submit_approved`; approval
leaves the launch approved without a queued job. Agents cannot use `mail.send`
or `mail.approve`. An agent mailbox `can_send` grant is only an ACL prerequisite
for `mail_submit_approved`; it does not permit direct sending. Marketing sends are
blocked unless current communication-basis evidence and sender/provider policy
checks pass. Suppressed addresses are skipped. A send with an uncertain provider
outcome remains `unknown` and is never resubmitted automatically. Office does
not run automatic email follow-ups or drip sequences.

### Finding your way around

The sidebar has seven everyday destinations: Dashboard, Companies, Campaigns,
Inbox, Templates, Meetings, and Settings. Open **Companies → Contacts** for the
contact directory, **Campaigns → Approvals** for human review, and
**Meetings → Meeting types** for booking configuration. Settings groups mail and
calendar connections, agents/MCP, imports, workspace/security, and review and
operations tools. Direct URLs and command search continue to reach these screens;
their access checks and human approval requirements remain enforced.

### Personalized company outreach

The primary campaign workflow groups company-specific drafts on a kanban board:
research, draft, human review, queued, SMTP accepted, and attention. Configure a
default marketing SMTP sender in Mail accounts, then reuse one campaign for many
companies. Each target keeps its own subject/body and template version; sender
and recipient bindings remain checked by the server. Only an independently
approved frozen message can enter the next ten-minute sending batch. The board
refreshes every two seconds and derives sending columns from actual outbound
records. Existing common-template bulk campaigns remain separately accessible.

See the [personalized sales guide](docs/runbooks/personalized-sales.md) for setup,
the 200-target group limit, review/queue behavior, and the `sales_campaigns_*` MCP
workflow. This changes campaign organization, not the approval or suppression
requirements.

### Writing email templates

Open **Templates**, choose a template, and create a version. Write directly in
the rendered email canvas or use **Edit HTML** to edit the same content as
source. Switching modes preserves the content. Insert the provided variables
and meeting button as needed, then use recipient preview to inspect sample
values before saving. Saved versions are immutable; create another version to
reuse and revise an existing one.

Agents can also create a reusable template container and immutable version
through MCP. The [template authoring runbook](docs/runbooks/mcp-template-authoring.md)
covers the create → version → render → draft path. Creating or previewing a
template does not create or send a message.

Office generates the plain-text email alternative from the canvas. If an
imported version already has a different plain-text alternative, the editor
shows a notice and preserves it until you edit the canvas or explicitly choose
the generated text.

### Choosing a meeting type

First configure an internal calendar under **Settings → Calendars**, then create
a meeting type and host availability under **Meetings → Meeting types**. Booking links,
campaigns, and email drafts offer active meeting types. Creating or updating a
type refreshes these selectors together; an empty selector provides a link to
setup, and a failed request offers a retry. The creation form uses the signed-in
host's calendar.

Lists and selectors show record names and status rather than raw UUIDs. Where a
name is unavailable, Office says so; identifiers remain available only where
needed for navigation or explicit technical details.

## Quick local start

Install the versions listed in [Local development](docs/runbooks/local-development.md):
Rust 1.99.0, Node.js 22 or newer, pnpm 11.24.0, PostgreSQL 17.11, and optionally
Redis 8. The repository does not include a database or Redis launcher.

Clone the project and enter its directory first:

```sh
git clone https://github.com/dev-thug/office-specify-app.git
cd office-specify-app
```

Create two separate local databases, copy the environment template, and edit
`DATABASE_URL` for your local PostgreSQL role. The example already points at
the `office_dev` database created below:

```sh
createdb office_dev
createdb office_test
cp .env.example .env
```

If you are not running Redis locally, set `REDIS_URL=` in `.env` before
starting the API.

Load `.env` and apply migrations. The Rust executables read process environment
variables; they do not load `.env` themselves. Keep `APP_ENV=development`
explicitly set and create the initial Admin with a password read from stdin:

```sh
set -a
source .env
set +a
cargo run -p cli -- migrate
cargo run -p cli -- bootstrap \
  --workspace-name "Office Dev" \
  --admin-email admin@example.test \
  --admin-password-stdin
```

Start the API, worker, and web app in separate terminals, loading `.env` in each.
The web server proxies API requests to `API_ORIGIN` (default
`http://127.0.0.1:8080`) and listens at the local test origin
`http://127.0.0.1:13000`:

```sh
set -a; source .env; set +a; cargo run -p api
```

```sh
set -a; source .env; set +a; cargo run -p worker
```

```sh
pnpm install --frozen-lockfile
set -a; source .env; set +a
API_ORIGIN=http://127.0.0.1:8080 pnpm --dir apps/web dev --hostname 127.0.0.1 --port 13000
```

Open [http://127.0.0.1:13000](http://127.0.0.1:13000) and sign in with the
bootstrap account. The optional MCP endpoint listens at
`http://127.0.0.1:8081/mcp`:

```sh
set -a; source .env; set +a; cargo run -p mcp
```

Use an HTTP MCP client with an Office service credential in its Authorization
Bearer header. OAuth discovery and stdio are not implemented. Agent scopes and
mailbox grants still apply; human approval and direct-send restrictions remain
in force. See the [agent guide](docs/runbooks/mcp-agent-guide.md) before issuing
credentials or submitting company research.

These are local-only addresses. A booking or unsubscribe URL using loopback can
only be opened on the machine running Office. Local work does not need a public
tunnel. For a future deployment, set `PUBLIC_BASE_URL=https://office.specify.app`
in the API and worker environments, route that HTTPS origin through a reverse
proxy to the Next.js web app, and route its `/api/v1`, `/public/v1`, and
`/webhooks/v1` paths to the Rust API. Set the Next.js `API_ORIGIN` to the
deployment's private API origin. See the [local-development runbook](docs/runbooks/local-development.md)
for the workspace URL override and optional OAuth callback setup.

## Configuration and deployment

`API_ADDR` controls where the Rust API listens. `API_ORIGIN` controls where the
Next.js server sends proxied API requests; keep them aligned. `PUBLIC_BASE_URL`
is the browser-visible Office origin used when generating recipient booking and
unsubscribe links. An existing `workspaces.settings.public_base_url` value
overrides the environment value for that workspace. Clear or update old
workspace overrides when changing domains so newly generated links use the
intended origin. Previously issued capability links retain their original
origin; recreate affected links or content after a domain change.

`APP_ENV` accepts `development`, `test`, or `production`; when unset, services
use production behavior. Production requires Redis, a persistent HMAC key of
at least 32 bytes, an AES-256 SecretStore key, and an HTTPS public origin where
recipient links must work. Supply private values through the deployment's
secret manager. Keep the HMAC and encryption keys stable and backed up alongside
the encrypted SecretStore. See the [SMTP runbook](docs/runbooks/smtp.md) and
[database restore runbook](docs/runbooks/restore.md) before operating a
deployment.

Local link and tracking tests use `http://127.0.0.1:13000`; no public tunnel is
needed. Gmail's image proxy cannot fetch a pixel from a loopback address, and
image or link requests from deployed proxies and scanners are engagement
requests, not proof that a person read the message. Use a reachable HTTPS
deployment origin such as `https://office.specify.app` for recipient-facing
links.

The app and internal local calendar can run without OAuth. A fresh workspace
has no Office mail account, and the supported way to provision one is
**Settings → Mail accounts → Connect mailbox** through Zoho Mail OAuth. This
connection provisions the Office mailbox for correspondence and Inbox access;
it does not provide SMTP credentials. The connecting Admin gets an initial
mailbox grant; another Admin setting up SMTP needs `can_send` on the selected
account, granted through **Manage grants**. Office campaigns use the separately
configured SMTP relay credentials. Configure the exact Zoho Mail callback URL
and server-side client credentials when connecting Zoho Mail. None of the local
app, local calendar, or SMTP setup requires a public tunnel. For deployed
recipient booking and unsubscribe links, `PUBLIC_BASE_URL` must use a reachable
HTTPS origin. Do not treat mock or local SMTP acceptance as proof of inbox
delivery, sender authentication, or live Zoho behavior.

The optional SMTP-profile inbox monitor authenticates directly to the configured
IMAP host; it does not require Zoho Mail OAuth or provision an Office mailbox.
The SMTP profile must still be attached to an existing Office `mail_account_id`.

## Current boundaries

Office is designed to fail closed around outbound effects. The former native
Zoho Campaigns integration is retired. The regular upgrade migration disables
persisted Campaigns connections and channels and retires their queued jobs;
historical migrations remain in the normal install and upgrade sequence.
Office no longer submits Zoho Campaigns campaigns, exports contacts or content,
runs scheduled Campaigns synchronization, imports or propagates external
opt-outs, or exposes a Campaigns webhook route. Office campaigns continue over
authenticated SMTP, and Office-local unsubscribe and booking links remain
active. The internal calendar stores Office booking data and does not create
external calendar invitations. Before production use, verify current consent
policy, relay sender authorization, inbox delivery, and backup/restore behavior.

## License

Office is available under the [MIT License](LICENSE).
