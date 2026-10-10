# Office

Office is an open-source, self-hosted workspace for contacts, companies, email
templates and campaigns, meeting bookings, approvals, and operational controls.
The backend is a Rust workspace; the web interface is a Next.js app in
[`apps/web`](apps/web/).

## Start here

- [Local development](docs/runbooks/local-development.md) — prerequisites,
  database setup, migrations, first admin, and service startup.
- [SMTP setup](docs/runbooks/smtp.md) — configure Office-owned campaigns to send
  through an authenticated SMTP relay.
- [Local booking setup](docs/runbooks/local-booking.md) — set up the internal
  calendar, availability, meeting types, and private booking links.
- [Zoho Campaigns setup](docs/runbooks/zoho-campaigns.md) — optional native
  Campaigns connection, separate from SMTP.
- [Unknown-send reconciliation](docs/runbooks/unknown-send-reconciliation.md)
  and [database restore](docs/runbooks/restore.md) — operator recovery guides.
- [Source design](docs/design/source/outreach-os-design-v1.0.md),
  [API and MCP contracts](docs/design/integration-contracts.md), and the
  [acceptance matrix](docs/design/test-matrix.md) — implementation constraints
  and evidence.

## Features

New to Office? Open **Usage guide** in the top bar for the setup-to-send flow
and instructions for every menu. Hover or focus the question-mark icon beside
a menu or the current screen name for a short explanation; click it (or press
Enter) for when to use the feature and numbered steps. On touch screens, tap
the same icon. Escape closes the instructions and returns focus to the button.

| Area | What Office provides |
| --- | --- |
| CRM | Contact and company records, search, notes, activity, and CSV imports. |
| Email content | One rendered email canvas with a toggle to HTML source, variable presets and schema, recipient preview, and reusable immutable template versions. |
| Office campaigns | Contact selection, consent and suppression checks, preflight, human approval, and one-step dispatch through a configured channel. SMTP campaigns are composed and recorded by Office; SMTP setup requires an active Office mail account. |
| Zoho Campaigns | An optional, independent route that exports an approved launch to Zoho Campaigns. It is not needed for SMTP delivery. |
| Inbox | Mailbox-bound Zoho Mail correspondence, kept separate from campaign transport. |
| Meetings | Internal meeting types, host availability, booking links, and persisted booking, reschedule, and cancellation records. The local calendar does not use OAuth. |
| Access and operations | Admin, Manager, Member, Agent, and System principals; mailbox grants; approvals; emergency stop; job and delivery status; audit records; and recovery runbooks. |
| MCP | A Streamable HTTP endpoint with workspace-scoped service credentials and policy-checked tools for CRM research, proposals, drafts, bookings, and campaign workflows. Agents cannot directly submit arbitrary email or decide approvals. |

Office keeps an explicit separation between preparing an action and carrying it
out. Agent-created email stays a draft for human approval. Marketing sends are
blocked unless current communication-basis evidence and sender/provider policy
checks pass. Suppressed addresses are skipped. A send with an uncertain provider
outcome remains `unknown` and is never resubmitted automatically. Office does
not run automatic email follow-ups or drip sequences.

### Writing email templates

Open **Templates**, choose a template, and create a version. Write directly in
the rendered email canvas or use **Edit HTML** to edit the same content as
source. Switching modes preserves the content. Insert the provided variables
and meeting button as needed, then use recipient preview to inspect sample
values before saving. Saved versions are immutable; create another version to
reuse and revise an existing one.

Office generates the plain-text email alternative from the canvas. If an
imported version already has a different plain-text alternative, the editor
shows a notice and preserves it until you edit the canvas or explicitly choose
the generated text.

### Choosing a meeting type

First configure an internal calendar under **Settings → Calendars**, then create
a meeting type and host availability under **Meeting types**. Booking links,
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

The app and internal local calendar can run without OAuth. A fresh workspace
has no Office mail account, though, and the current supported way to provision
one is **Settings → Mail accounts → Connect mailbox** through Zoho Mail OAuth.
An SMTP login does not create this Office account. The connecting Admin gets an
initial mailbox grant; another Admin setting up SMTP needs `can_send` on the
selected account, granted through **Manage grants**. SMTP uses its own relay
credentials after that mailbox prerequisite is met. This Zoho Mail OAuth flow
is separate from Zoho Campaigns OAuth, which SMTP does not require. Configure
the exact Zoho callback URL and server-side client credentials for whichever
OAuth connection you enable. None of the local app, local calendar, or SMTP
setup requires a public tunnel. The native Zoho Campaigns bulk route does
require an externally reachable HTTPS origin for Zoho to fetch Office's frozen
HTML. Do not treat mock or local SMTP acceptance as proof of inbox delivery,
sender authentication, or live Zoho behavior.

## Current boundaries

Office is designed to fail closed around outbound effects. Campaign webhook
verification and external calendar sync remain release gates; configured OAuth
does not by itself verify a live account, sender identity, delivery, or booking
behavior.
The internal calendar stores Office booking data and does not create external
calendar invitations. Marketing policy, provider permission, sender
authentication, OAuth-client compatibility, and backup/restore release checks
must be reviewed before production use. See the [source design's release
gates](docs/design/source/outreach-os-design-v1.0.md) for the required
evidence.

## License

Office is available under the [MIT License](LICENSE).
