# Office campaigns over SMTP

Office stores templates and immutable versions, renders each recipient's HTML and
plain text, and owns campaign approval, dispatch, booking links and unsubscribe
records. An authenticated SMTP relay transports the resulting message. This path
does not create or send a Zoho Campaigns campaign.

## Configure a connection

In Settings → Mail accounts, create an SMTP connection for the Office mailbox
whose permissions govern the campaign. Supply a TLS relay host, port, username,
private password, From address and optional Reply-To. Confirm that the From
address is authorized by the relay. Credentials are write-only and stored in the
server's encrypted SecretStore. Never commit them or include them in support
screenshots.

For a US paid Zoho Mail account, the documented relay is `smtppro.zoho.com`, using
implicit TLS on port 465 or STARTTLS on port 587. Use the server shown for the
actual account and data center. With MFA, use a dedicated app password generated
by the account owner. A subdomain From address must be registered as a Mail
account/alias; verification in Zoho Campaigns alone does not establish that.
See [Zoho SMTP settings](https://www.zoho.com/mail/help/zoho-smtp.html) and
[Mail aliases](https://www.zoho.com/mail/help/adminconsole/user-settings.html#email-alias).

Connection verification tests TLS and authentication without sending a message.
It does not prove alias authorization or inbox delivery. Validate both with an
explicitly approved message to a controlled recipient. Disabling a connection
prevents subsequent dispatches through it.
Profiles are immutable. If you revoke or rotate the relay password, disable the
old connection and create a replacement with the new app password.

## Public URLs and headers

Set the workspace public application origin to your deployed HTTPS domain before
freezing a campaign. Recipients receive Office's signed unsubscribe and booking
links. Opening an unsubscribe page does not change state; explicit confirmation
records the Office suppression. The one-click endpoint accepts the corresponding
unsubscribe POST. Keep these public routes reachable through your reverse proxy.
No fixed deployment domain is required by this open-source application.

Office constructs From, Reply-To, Subject, Message-ID, multipart bodies, its
correlation header, and recipient-specific List-Unsubscribe headers. Profile
custom headers cannot override the generated unsubscribe/correlation values or
structural and server-trust headers. The current profile custom-header allowlist
is `X-Auto-Response-Suppress` and `X-Importance`. SMTP relays and recipient servers can add or
rewrite Return-Path, Received, DKIM, ARC and Authentication-Results. Compare the
received raw message with the intended headers before claiming preservation.
For [RFC 8058](https://www.rfc-editor.org/rfc/rfc8058.html) mailbox-provider one-click handling, check that the relay's valid
DKIM signature covers both unsubscribe headers. Providing the endpoint and
headers alone does not guarantee the recipient mailbox displays that action.

An SMTP acceptance response means the relay accepted the message, not that the
recipient received it. A lost final response is an unknown outcome and must not
be automatically resent. SMTP has no reliable sent-message lookup for resolving
that ambiguity; retain the unknown record for operator investigation.
