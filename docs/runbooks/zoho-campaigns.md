# Zoho setup and recovery

Office separates Zoho Mail, Campaigns Free OAuth v1.1, and the optional prepaid
Campaigns Email API v2. See [implementation status](../design/zoho-integration-status.md)
for application-wired features versus SDK-only methods. No current mock test
establishes successful live-account connection or delivery.

## Fix an Invalid Client page

1. Register a **Server-based Application** in the US [Zoho API Console](https://api-console.zoho.com/).
2. Copy the exact callback URI shown in Office Settings → Mail accounts → Zoho Campaigns into the client registration. Keep scheme, host, port and path identical. Production callbacks require public HTTPS.
3. Put `ZOHO_CAMPAIGNS_CLIENT_ID` and `ZOHO_CAMPAIGNS_CLIENT_SECRET` in the private API/worker environment. Optionally set `ZOHO_CAMPAIGNS_REDIRECT_URI` to that exact registered URI.
4. Restart the API and worker with that environment loaded. The binaries do not automatically read `.env`. Missing/invalid settings disable the authorization button instead of opening a URL without a client ID.
5. As a human workspace administrator, start a fresh connection and consent. The current Campaigns scopes are read-only. OAuth state expires after ten minutes, is bound to the starting session/workspace, and is consumed once.
6. Check the connected mailing lists and recent campaign metadata. A successful check confirms those read permissions only; it does not enable marketing sends or verify quota/sender/domain settings.

Do not paste client secrets, refresh tokens, API keys or callback codes into chat,
Git, screenshots, issue bodies or browser developer tools. Tokens are stored in
the encrypted server SecretStore. Production also requires server cryptographic
keys and Redis as described in `.env.example`.

## Mail is a different connection

Set `ZOHO_MAIL_CLIENT_ID`, `ZOHO_MAIL_CLIENT_SECRET` and the registered
`ZOHO_MAIL_REDIRECT_URI`. Mail OAuth discovers the real account and binds Inbox
polling and correspondence send/reply to that workspace mailbox. It is not a
marketing sender. In-flight OAuth requests started with an older callback URI
must be restarted after a configuration change.

Unknown send outcomes are never automatically retried. Zoho Mail does not
provide this adapter with reliable lookup by Office correlation ID, so inspect
the account's sent mailbox before deciding any manual recovery.

## Optional remote template library

The documented template CRUD API belongs to the separate prepaid Email API v2
organization, not the existing Campaigns Free OAuth library. Zoho must enable
that product. Office does not sign up, change the existing organization, buy
credits or enable it automatically.

To enable an already-provisioned account, set `ZOHO_EMAIL_API_ENABLED=true`,
`ZOHO_EMAIL_API_KEY` and `ZOHO_EMAIL_API_WORKSPACE_ID` on the server. The key is
available only to human administrators in that exact workspace. Open a local
Office template's detail screen and select **Zoho templates**.

Prefer copying into a new immutable Office version. Map every Zoho merge field
explicitly. The basic Office editor cannot represent all native HTML; compatible
conversion is explicit and does not edit the remote source. The isolated native
preview blocks external network resources, so remotely hosted images are not
loaded. Browser/email-client rendering can differ.

Remote update/delete requires the current content hash. Office serializes its
own writes across API processes; external Zoho clients can still race because
the provider has no atomic compare-and-swap. If a write outcome is unknown,
refresh and inspect the remote library before explicitly reconciling it. Never
blindly repeat create/send operations after a timeout.

## Marketing delivery remaining gate

Campaigns contact/campaign/report SDK methods are present, but the approved bulk
export, native opt-out synchronization, approval-bound send, and report polling
application path is not enabled. Production marketing delivery remains disabled.
See the [Free Campaigns delivery plan](../superpowers/plans/2026-10-10-free-campaigns-delivery-plan.md)
for the exact account-contract checks and implementation dependencies. Office
booking and unsubscribe endpoints remain local; Campaigns requires its native
unsubscribe mechanism as well. A paid Zoho Mail plan does not upgrade Campaigns.
