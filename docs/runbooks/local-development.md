# Local development

This runbook starts the current repository locally. The Rust binaries read configuration from the process environment; they do not load `.env` themselves. Keep a dedicated PostgreSQL database for tests so test fixtures never share development data.

## Requirements

- Rust stable with edition 2024 support (the workspace declares Rust 1.99).
- Node.js 22 or newer and pnpm 11.24.0, as declared by the root and web package manifests.
- PostgreSQL 17 with permission to create the database, `btree_gist` extension, and runtime roles created by the migrations.
- Redis is optional for development. When `REDIS_URL` is set, the API expects Redis to be reachable at startup. If Redis is unavailable, leave `REDIS_URL` empty in development; rate limiting is then disabled.

The repository has no Docker Compose file or local database/Redis launcher. Start PostgreSQL and, when used, Redis with the service manager for your machine.

## Configure databases and environment

Create separate databases using a local PostgreSQL owner. These commands assume the default local PostgreSQL connection for your account:

```sh
createdb outreach_dev
createdb outreach_test
```

Create a local environment file and load it in every shell that runs a Rust process:

```sh
[ -f .env ] || cp .env.example .env
set -a
source .env
set +a
```

The example selects `APP_ENV=development` and `DATABASE_URL=postgres://hyunjoong@localhost:5432/outreach_dev`. Change `DATABASE_URL` if your local PostgreSQL user or database differs. Rust configuration treats an unset `APP_ENV` as production, so keep the explicit development value. If you do not run Redis, set `REDIS_URL=` in `.env` before sourcing it.

Rust services use defaults from [`config.rs`](../../crates/infrastructure/src/config.rs): API at `127.0.0.1:8080`, MCP at `127.0.0.1:8081`, objects under `var/objects`, and secrets under `var/secrets.json`. The web app rewrites API paths to `API_ORIGIN`, defaulting to `http://127.0.0.1:8080`. Set `PUBLIC_BASE_URL` to the browser-visible Next.js origin (default local example: `http://localhost:3000`) in both API and worker shells so generated booking/unsubscribe links open the public pages. Production requires an explicit HTTPS origin.

## Initialize and start

Apply migrations and create the first local admin. `migrate` uses the configured `DATABASE_URL`; `bootstrap` reads the password from stdin without placing it in shell history or process arguments:

```sh
cargo run -p cli -- migrate
cargo run -p cli -- bootstrap \
  --workspace-name "Outreach Dev" \
  --admin-email admin@example.test \
  --admin-password-stdin
```

Enter a password of 12–128 characters when prompted. Bootstrap reuses the first workspace if one already exists. It starts with the emergency stop enabled, so sending is blocked until an administrator deliberately changes that setting.

Start each service in a separate shell, sourcing `.env` in each one:

```sh
cargo run -p api
```

```sh
cargo run -p worker
```

```sh
pnpm install --frozen-lockfile
pnpm --dir apps/web dev
```

Open `http://localhost:3000` and sign in with the bootstrap account. The MCP server is optional for the web app; to run it locally, use a fourth shell:

```sh
cargo run -p mcp
```

MCP uses service-credential bearer authentication. OAuth discovery is not implemented, and the default allowed-origin list is empty.

For an additional synthetic workspace, the CLI exposes a development/test-only command behind a feature flag:

```sh
cargo run -p cli --features testing -- seed-demo
```

Each invocation creates another workspace and prints fixture identities. Use this only against a disposable development database.

## Run checks

The database-backed Rust test helpers migrate the database named by `DATABASE_URL`. Point it at the dedicated test database before running them:

```sh
APP_ENV=test DATABASE_URL=postgres://hyunjoong@localhost:5432/outreach_test cargo test --workspace
```

Then run the web checks defined by `apps/web/package.json`:

```sh
pnpm --dir apps/web typecheck
pnpm --dir apps/web lint
pnpm --dir apps/web lint:design
pnpm --dir apps/web test
```

The web package does not currently define a Playwright test script. See the test matrix for requirement-level coverage; frontend unit tests do not substitute for the server-side and PostgreSQL checks.

The CLI also generates the contracts from the current code:

```sh
cargo run -p cli -- openapi
cargo run -p cli -- mcp-schema
```

These write `contracts/openapi/openapi.json` and `contracts/mcp/tools.json` respectively.

For the full operator command list, run `cargo run -p cli -- --help`. Privacy-erasure and object-cleanup commands default to preview; they require an explicit `--execute` flag to change data. Use those commands only against the intended workspace/database.

## Fake provider behavior

Explicit `Config::test()` fixtures and an unconfigured `APP_ENV=dev` server use fake providers. Other runtime configurations use the mailbox-bound Zoho Mail router and fail closed when credentials or account authorization are unavailable. `APP_ENV=test` alone does not enable fake external authentication. Real Calendar and marketing delivery adapters remain disabled in runtime. A fake `accepted` result means only that the in-process fake accepted the request. It does not mean delivered or read. The default fake mail outcome is accepted; supported `FAKE_PROVIDER_SCRIPT` outcome names and JSON shape are documented in [`fake.rs`](../../crates/infrastructure/src/providers/fake.rs).

Fake state is in memory and private to each process. Restarting the worker loses its fake sent-message lookup history. The development inbound endpoint at `POST /api/v1/dev/fake-mail/inbound` injects into the API process's fake instance; a separate worker process has a different instance and will not see that injected message. The worker loads both fake mail and fake delivery outcome queues from `FAKE_PROVIDER_SCRIPT` when its provider context starts. Do not use these fakes to claim a real provider integration or to rehearse cross-process recovery.

The implementation spec keeps external-provider, legal, security, and backup/restore release gates closed. See §20 of the source design before treating any integration as production-ready.

## Zoho setup

Register a server-based application in the US Zoho API Console. Set the client
ID, client secret and exact redirect URI on the server; start the binaries with
the private environment loaded (they do not automatically load `.env`).
Use Settings → Mail accounts for the Campaigns connection setup/checklist.
Mail and Campaigns have separate `ZOHO_MAIL_*` and `ZOHO_CAMPAIGNS_*` settings.
OAuth state is session/workspace bound and single-use; refresh tokens are stored
encrypted. Missing configuration must be fixed before opening a Zoho consent URL.

Campaigns Free supports the regular v1.1 API, not the separate Email API v2
prepaid template store. Optional template CRUD requires all of
`ZOHO_EMAIL_API_ENABLED=true`, `ZOHO_EMAIL_API_KEY`, and
`ZOHO_EMAIL_API_WORKSPACE_ID`. Keys stay on the server. This does not sign up,
purchase credits or upgrade an existing Campaigns organization.
Template write outcomes that cannot be confirmed are not automatically retried;
refresh the remote library and reconcile before using a new operation key.
See the [implementation status](../design/zoho-integration-status.md) for what is
actually wired versus SDK-only or still pending.
