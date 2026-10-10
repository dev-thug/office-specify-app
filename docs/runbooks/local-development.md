# Local development

This runbook starts the repository on one development machine. The API, worker,
and MCP executables read the process environment; they do not load `.env`
themselves. Use an isolated database for tests and keep local public links on
the loopback web origin `http://127.0.0.1:13000`. No public tunnel is needed.

## Requirements

- Rust 1.99.0, selected by [`rust-toolchain.toml`](../../rust-toolchain.toml).
- Node.js 22 or newer and pnpm 11.24.0, as declared in the root and web package
  manifests.
- PostgreSQL 17.11 with `btree_gist`; migrations also create the runtime roles
  used by the API and worker. Use a local database owner that can create the
  database and required extension/roles.
- Redis 8 is optional for development. If `REDIS_URL` is non-empty, the API
  connects to Redis when it starts. For local work without Redis, set
  `REDIS_URL=`. Rate limiting is then disabled in development. Production
  requires Redis and fails closed without it.

The repository does not include a Docker Compose file, database bootstrap
service, or Redis launcher. Start PostgreSQL and, if used, Redis with the
service manager for your machine. The versions above are the versions recorded
by the workspace's source design and manifests; they are not installer
commands.

## Create local databases and load configuration

Keep development and test data in separate databases:

```sh
createdb office_dev
createdb office_test
```

Copy the environment example, then edit `DATABASE_URL` if your local PostgreSQL
username or database name differs. Keep `APP_ENV=development` set explicitly;
an unset value defaults to production behavior, while an unknown value stops
startup.

```sh
cp .env.example .env
```

In every terminal that runs a Rust service or CLI command, load the environment
like this:

```sh
set -a
source .env
set +a
```

The checked-in example points at `office_dev`; change the `db_user` role name
to match your local PostgreSQL role, for example
`postgres://db_user@localhost:5432/office_dev`.
The example sets `PUBLIC_BASE_URL=http://127.0.0.1:13000`. This is the public
Next.js origin used inside booking and unsubscribe links. It must match the
origin where your browser opens Office.

## Configure the web-to-API connection

`API_ADDR` is the address the Rust API binds to; its default is
`127.0.0.1:8080`. `API_ORIGIN` is read by the Next.js server and is the target
for its same-origin rewrites of `/api/v1`, `/public/v1`, and `/webhooks/v1`.
Set `API_ORIGIN` to the same address as `API_ADDR` if you change the default.
The web server listens at `127.0.0.1:13000` in the command below. MCP defaults
to `127.0.0.1:8081`.

For recipient-facing URLs, an existing non-empty
`workspaces.settings.public_base_url` takes priority over `PUBLIC_BASE_URL`.
The setting is read when Office creates capability URLs and frozen campaign
content. If the workspace predates this local origin and still has an older
value, update or clear that workspace setting before creating links. This
database property has no operator CLI command; use a PostgreSQL owner connection
to update only the intended workspace.

First list the local workspace IDs, then paste the intended ID into the shell
when prompted. The SQL uses a psql parameter for the workspace ID instead of
embedding one in the statement:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
  -c 'SELECT id, name FROM workspaces ORDER BY name, id;'
# Paste the intended workspace UUID from the query above, then press Return.
read -r WORKSPACE_ID
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -v workspace_id="$WORKSPACE_ID" <<'SQL'
BEGIN;
UPDATE workspaces
   SET settings = jsonb_set(
       COALESCE(settings, '{}'::jsonb),
       '{public_base_url}',
       to_jsonb('http://127.0.0.1:13000'::text),
       true
   )
 WHERE id = :'workspace_id'::uuid
RETURNING id, settings->>'public_base_url' AS public_base_url;
COMMIT;
SQL
```

To remove the override and return to the environment fallback, use the same
`WORKSPACE_ID` and owner connection:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -v workspace_id="$WORKSPACE_ID" <<'SQL'
BEGIN;
UPDATE workspaces
   SET settings = COALESCE(settings, '{}'::jsonb) - 'public_base_url'
 WHERE id = :'workspace_id'::uuid
RETURNING id, settings->>'public_base_url' AS public_base_url;
COMMIT;
SQL
```

Changing the setting affects links created afterward. A previously issued
booking, unsubscribe, or invitation URL already contains its original origin;
recreate it after changing origins. Treat booking and unsubscribe capability
URLs as private. A loopback link only works from the computer running Office.

## Apply migrations and create the first admin

The API does not migrate the database at startup. Run the CLI migration command
after selecting the intended development database. Then create the first
workspace and Admin account. The password is read from stdin without echo and
must be 12–128 characters; do not pass it as a command-line argument.

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

Bootstrap reuses the first workspace if one already exists. A new workspace
starts with the emergency stop enabled, so outbound sends remain blocked until
an Admin deliberately changes the setting. Keep local demo data and credentials
out of a production database.

## Start the services

Run the API and worker in separate terminals. Each terminal must load the same
database and relevant link configuration from `.env`:

```sh
set -a
source .env
set +a
cargo run -p api
```

```sh
set -a
source .env
set +a
cargo run -p worker
```

Install the locked web dependencies once, then run Next.js on the local test
port. `API_ORIGIN` must match the API's `API_ADDR`:

```sh
pnpm install --frozen-lockfile
set -a
source .env
set +a
API_ORIGIN=http://127.0.0.1:8080 \
  pnpm --dir apps/web dev --hostname 127.0.0.1 --port 13000
```

Open [http://127.0.0.1:13000](http://127.0.0.1:13000) and sign in with the
bootstrap account. The web app is the browser-facing origin; API requests are
proxied to the Rust API so session and CSRF cookies stay same-origin.

The MCP server is optional for the web app. Start it in another terminal when
you want to connect an MCP client:

```sh
set -a
source .env
set +a
cargo run -p mcp
```

It listens on `http://127.0.0.1:8081/mcp` by default. MCP uses bearer service
credentials rather than OAuth discovery. Create a scoped credential through
Office's agent/account settings and grant only the required tools and mailbox
scope. The default MCP browser-origin allowlist is empty. MCP agents can prepare
drafts and workflow requests; Office permissions and human approvals still
apply, and the MCP catalog has no direct arbitrary-email submit or approval
decision tool.

## Configure an eventual hosted origin

For a deployment at `office.specify.app`, set `PUBLIC_BASE_URL` to
`https://office.specify.app` in both API and worker environments. Terminate TLS
at a reverse proxy and route the public origin to Next.js; configure Next.js
`API_ORIGIN` to the private Rust API address and route the rewritten API paths
to that service. No tunnel is part of the local workflow.

CRM, the internal calendar, and local booking work without OAuth. To set up
SMTP in a fresh workspace, first provision an Office mail account through
**Settings → Mail accounts → Connect mailbox** using Zoho Mail OAuth; a bootstrap
workspace has no mailbox and SMTP relay credentials do not create one. The
current repository has no standalone SMTP-only mailbox-creation route. See the
[SMTP runbook](smtp.md) for the active-account and `can_send` requirements.

For any Zoho OAuth connection, register the exact full callback URI used by
Office in the Zoho client and set the matching server value. With the example
domain, the paths are:

- Zoho Mail: `https://office.specify.app/api/v1/integrations/zoho/callback`
- Zoho Campaigns: `https://office.specify.app/api/v1/integrations/zoho-campaigns/callback`

Configure `ZOHO_MAIL_REDIRECT_URI` or `ZOHO_CAMPAIGNS_REDIRECT_URI` alongside
the corresponding server-side client credentials. The new SMTP mailbox
provisioning flow uses the Zoho Mail callback. The SMTP relay itself uses
separate SMTP credentials and does not require Zoho Campaigns OAuth. The local
internal calendar does not use either callback.

Production also requires a persistent `OUTREACH_HMAC_KEY` of at least 32 bytes,
a 64-character-hex `OUTREACH_SECRET_KEY` for AES-256 SecretStore encryption, and
a reachable `REDIS_URL`. Generate and store these values with your secret
manager; never copy private values into `.env.example`, source control, issue
reports, or screenshots. Keep the keys stable and include the encrypted
SecretStore file in the deployment's protected backup plan. Development/test
fallback keys are fixed for local convenience and are not production secrets.

## Optional development fixtures

The CLI's `seed-demo` command requires its `testing` feature and refuses to run
unless `APP_ENV` is `development` or `test`. It creates an additional synthetic
workspace and prints fixture identities. Use it only with a disposable local
database:

```sh
cargo run -p cli --features testing -- seed-demo
```

Fake provider data is in memory and private to each process. A fake `accepted`
result means the local fake accepted an action; it is not evidence of actual
mail delivery. Real Zoho Mail correspondence uses a separate integration from
Office SMTP campaign delivery. The [SMTP runbook](smtp.md) covers controlled
inbox verification and unknown-send handling.

## Checks and generated contracts

If you choose to run the database-backed Rust suite, set `DATABASE_URL` to the
separate `office_test` database first. The tests must never share a development
or live database. Replace `db_user` with the local PostgreSQL role you use in
your connection string:

```sh
APP_ENV=test DATABASE_URL=postgres://db_user@localhost:5432/office_test cargo test --workspace
```

The web checks declared in `apps/web/package.json` are:

```sh
pnpm --dir apps/web typecheck
pnpm --dir apps/web lint
pnpm --dir apps/web lint:design
pnpm --dir apps/web test
```

The CLI can export the current OpenAPI and MCP tool contracts:

```sh
cargo run -p cli -- openapi
cargo run -p cli -- mcp-schema
```

These commands write `contracts/openapi/openapi.json` and
`contracts/mcp/tools.json`. Run any write command only with the intended local
database selected. See the [acceptance matrix](../design/test-matrix.md) for
requirement-level evidence and remaining release gates.

## Zoho integration notes

Register a Zoho server-based application only if you intend to connect that
provider. Configure its client ID, client secret, and exact callback on the
server before starting OAuth from Settings. Zoho Mail and Zoho Campaigns use
separate credentials and scopes. OAuth state is workspace/session bound, and
refresh tokens are stored encrypted. Do not use Campaigns credentials as SMTP
credentials.

Campaigns Free uses its regular v1.1 API. The optional prepaid Zoho Email API
template store is a different service, disabled by default, and not part of the
Campaigns Free send flow. See the [Zoho Campaigns runbook](zoho-campaigns.md)
for its external setup and the [integration status](../design/zoho-integration-status.md)
for supported capability boundaries.
