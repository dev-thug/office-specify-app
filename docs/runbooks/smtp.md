# Office campaigns over SMTP

Office owns the campaign, template version, recipient snapshot, preflight,
approval, per-recipient rendering, unsubscribe record, and dispatch state. An
authenticated SMTP relay transports each prepared message. This workflow is
independent of Zoho Mail OAuth, which provisions Office mailboxes and Inbox
correspondence.

SMTP credentials and sender fields have different jobs:

- The SMTP **username and password** authenticate the connection to the relay.
- **From** is the registered relay sender address or an alias that relay permits
  the account to use. An Admin must attest to that authorization while creating
  the connection. The login address does not automatically authorize a
  different From address.
- **Reply-To** is optional and controls where a recipient's reply is directed.
  It is not the SMTP login and does not authorize the From address.

## Configure the relay connection

An SMTP profile attaches to an existing `mail_account_id`; its relay username
and password do not create an Office mailbox. A newly bootstrapped workspace
has no mail account, and this repository has no SMTP-only mail-account creation
route. The current supported way to provision a new mailbox is Zoho Mail OAuth
from **Settings → Mail accounts → Connect mailbox**. Configure
`ZOHO_MAIL_CLIENT_ID`, `ZOHO_MAIL_CLIENT_SECRET`, and the exact callback URI
(`ZOHO_MAIL_REDIRECT_URI`, or the configured public origin plus
`/api/v1/integrations/zoho/callback`) before starting that connection. This
creates an active Office mail-account record; the Admin who connected it gets
an initial mailbox grant that includes `can_send`.

The Admin creating or verifying the SMTP profile must have `can_send` on the
selected account. If a different Admin manages SMTP, an Admin can grant that
permission with **Manage grants** on the mailbox. The mailbox and its grant are
required even though actual campaign transport uses the independent SMTP
credentials. Zoho Mail OAuth is the supported mailbox-provisioning flow. If the
workspace already has an active Office mail account and the acting Admin has
`can_send`, use it without provisioning another one.

Once that prerequisite is met, an Admin opens **Settings → Mail accounts**,
adds an SMTP connection, and supplies the relay host, port, TLS mode, SMTP
username and password, From name/address, and optional Reply-To. The password
is write-only and stored encrypted in the server SecretStore. Do not commit it
or include it in screenshots or support reports.

Choose one of the supported TLS modes:

- `tls` for implicit TLS, commonly port 465.
- `starttls` for STARTTLS, commonly port 587.

Use the exact host and port listed by the relay for the account's region. For a
US paid Zoho Mail relay, the documented host is `smtppro.zoho.com`; Zoho
accounts using MFA generally need a dedicated app password. Confirm the From
address is set up as the mailbox sender or authorized alias in the mail
provider. Verification by a separate service does not prove Mail relay alias
authorization. See [Zoho SMTP settings](https://www.zoho.com/mail/help/zoho-smtp.html)
and [Zoho Mail aliases](https://www.zoho.com/mail/help/adminconsole/user-settings.html#email-alias).

When saving the profile, the Admin attests that the From address is a
registered sender or authorized alias. This is an operator attestation, not a
provider-side alias verification. The initial profile is pending. Select
**Verify** in the SMTP profile list to check TLS and SMTP authentication. This
check does not send a message and does not prove that the relay will accept the
From alias or that a message will reach an inbox. A successful check activates
the profile/channel; test actual delivery separately with an approved message
to an inbox you control.

If the provider rejects the username or password, confirm that you are using
SMTP credentials for the relay, not Zoho Mail OAuth credentials. With MFA,
check the provider's app-password requirement. If credentials have been
revoked or rotated, disable the old profile and create a replacement with the
new secret; profile values are immutable. Disabling a profile prevents new
dispatches through that connection.

Production must set a persistent `OUTREACH_SECRET_KEY` (64 hexadecimal
characters) so Office can encrypt the SMTP password in its file SecretStore.
Keep the key stable, protect it with the deployment secret manager, and include
the encrypted `SECRETS_FILE` in the protected backup plan. Development's fixed
fallback key is only for local testing.

## Send an Office campaign

1. Create or import the contacts in Office, and use the visual template canvas
   or HTML source editor to author the email. Templates have a declared
   variable schema and immutable versions; recipient preview substitutes sample
   data without changing the stored template.
2. Build an Office campaign using its SMTP delivery channel and one of the
   immutable template versions.
3. Run recipient preflight. Office checks current address status, communication
   basis, suppression, channel policy, and the emergency stop. Marketing stays
   blocked when required consent/provider policy evidence is missing.
4. Freeze the launch, review the exact recipient/content manifest, and obtain
   the required human approval and final confirmation. A successful approval
   records permission to proceed; it does not itself mean the relay accepted a
   message.
5. Keep the Office worker running with the same database and environment as
   the API while dispatch jobs are processed.

Office does not send automatic follow-ups or drip steps. The current campaign
route supports a one-step dispatch; scheduled follow-up automation is outside
this workflow. Consent checks, suppressions, approval, and emergency-stop rules
are enforced in Office. Campaign contact and unsubscribe state is not
synchronized with an external marketing service.

## Public origin and unsubscribe behavior

Set `PUBLIC_BASE_URL` in the API and worker environments before creating
recipient links or freezing a campaign. For current local testing, use
`http://127.0.0.1:13000` and run the web app at that exact origin. The link
opens an Office page and the web server proxies the public API path to Rust;
no public tunnel is required. A loopback URL only works on the machine running
Office. Do not send a local capability URL to an external recipient.

For a future deployment, use `PUBLIC_BASE_URL=https://office.specify.app`,
terminate TLS at the reverse proxy, and route the public link paths to the
Next.js app and Rust API as described in the
[local-development runbook](local-development.md). An existing non-empty
`workspaces.settings.public_base_url` value overrides the environment. Update
or clear an old override before freezing a campaign; changing the URL does not
rewrite already frozen HTML or capability URLs.

Each rendered Office campaign message includes a recipient-specific unsubscribe
link to the Office frontend. Opening the page is read-only; confirming
unsubscribe sends a request to the Rust API, records a workspace suppression
against the address/contact in Office's CRM, and prevents later Office campaign
sends to that address. The public one-click POST, when emitted by the channel,
records the same Office suppression. This is an Office-local process; the
current route does not propagate opt-outs to an external marketing service.

Office emits the body unsubscribe link for a loopback HTTP origin, but omits
[RFC 8058](https://www.rfc-editor.org/rfc/rfc8058.html)
`List-Unsubscribe` and `List-Unsubscribe-Post` headers unless the
configured origin is HTTPS. Gmail's native one-click control therefore is not
expected for local HTTP testing. HTTPS makes those headers eligible; it does
not guarantee Gmail will show the control. The SMTP relay must preserve both
headers and use a valid DKIM signature that covers them. Inspect a received raw
message in a controlled inbox to confirm the relay's actual headers and DKIM
signature before relying on native one-click behavior. Office cannot make a
mailbox provider display that UI if the relay's signature does not satisfy its
requirements.

Office constructs From, Reply-To, Subject, Message-ID, MIME parts,
recipient-specific unsubscribe headers where allowed, and its correlation
header. Custom SMTP headers are limited to `X-Auto-Response-Suppress` and
`X-Importance`; they cannot replace unsubscribe, correlation, or structural
headers. The relay and recipient server can add or rewrite Return-Path,
Received, DKIM, ARC, and Authentication-Results.

## Delivery evidence and recovery

An SMTP `250` response means the relay accepted the submission; it does not
prove inbox delivery or that the recipient read the message. If Office loses
the final submission response, it keeps the message outcome `unknown`. Office
never automatically resends an unknown send because the relay may already have
accepted it. Use the [unknown-send reconciliation runbook](unknown-send-reconciliation.md)
and investigate with the relay and a controlled mailbox. Do not create a
replacement message until a human resolves the original outcome.

The current Zoho SMTP API path does not establish RFC 8058 support on its own.
Verify the received message's `List-Unsubscribe` headers and that Zoho DKIM
signs them. A connection check validates transport and login only; it does not
prove sender authorization, DKIM coverage, inbox placement, or Gmail's native
one-click UI.
