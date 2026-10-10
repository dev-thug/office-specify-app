# Office internal calendar and local booking

The internal calendar stores Office meeting availability and booking changes in
Office's database. It does not connect to Google Calendar, Zoho Calendar, or
another provider. Setup uses no OAuth. The internal calendar does not send
external calendar invitations, synchronize outside events, or create automatic
reminder/follow-up messages.

## Before setup

Start PostgreSQL, apply migrations, bootstrap an Admin, and run the API, worker,
and web app as described in [Local development](local-development.md). For the
current local test, the browser origin and the public URL configuration must
both be `http://127.0.0.1:13000`. Set `PUBLIC_BASE_URL` in the API and worker
environment, and check whether the active workspace has an older
`workspaces.settings.public_base_url` override. The workspace value wins; the
runbook shows how to update or clear it safely with a psql parameter.

The default ports are API `127.0.0.1:8080`, web `127.0.0.1:13000`, and MCP
`127.0.0.1:8081`. Next.js `API_ORIGIN` must point to the API's `API_ADDR`.
Local URLs use loopback and are only reachable from the computer running Office;
do not use a tunnel or send local booking links to remote guests.

## Set up the internal calendar

1. Sign in as an Office Admin and open **Settings → Calendars**.
2. Select **Set up Office calendar**. It creates a local connection
   with the `office_calendar` provider; there is no consent screen or external
   calendar login.
3. Confirm the Office internal calendar appears as ready in Settings and is
   available when choosing a calendar for a meeting-type host.
4. The equivalent API operation is `POST /api/v1/calendar-connections/local`
   with an empty JSON object (`{}`). The web setup action handles the normal
   authenticated request for you.

The connection is a reference to Office's database-backed calendar behavior.
It is not a copy or mirror of another calendar, and it does not check for
conflicts on external calendars.

## Create a meeting type and availability

Open **Meetings → Meeting types**. In the **Meeting types** tab, select **New
meeting type**, then set:

- Name and optional guest-facing description.
- The signed-in host's active Office calendar connection. A new meeting type
  belongs to the authenticated user; the creation API does not accept another
  host ID.
- Meeting duration and the buffers before and after each slot.
- Minimum booking notice and the booking horizon in days.

Save the type. Next open the **Availability** tab, choose the host, and set the
time zone using an IANA name such as `Asia/Seoul` or `America/Los_Angeles`.
Add weekly local-time windows for each available weekday. Add date-specific
exceptions for blocked time or a one-time available window; blocked time takes
priority if exceptions overlap. Save the schedule before issuing a link.

Create a booking link from **Meetings** or the meeting-type row. Choose the
meeting type, maximum uses, and optional expiry. The link is displayed once so
it can be copied. It is a capability URL: anyone who has it can view booking
options and, for a completed booking, use its management link. Share it only
with the intended guest over a safe channel.

## What guests and hosts can do

The public booking page displays the meeting name, description, host display
name, duration, and available slots without exposing private calendar details.
The guest selects a time and provides their name, email, and time zone.
Office checks availability again when
the guest submits; an earlier slot listing is only a candidate, not a
reservation guarantee. If two guests race for the same time, the database
conflict guard accepts at most one booking for the overlapping host interval.

The guest's management link supports rescheduling or cancellation. Each change
uses the booking's current version so an old page cannot silently overwrite a
newer change. Hosts can review bookings in **Meetings**, record attendance, and
update availability for future bookings. Booking, reschedule, and cancellation
records are stored in Office's database. With `office_calendar`, those changes
do not invoke an external calendar provider.

Booking links are not meeting invites: Office does not add guests to a Google
or Zoho event, send a calendar attachment, or synchronize changes to those
services. Any email campaign you separately send through SMTP remains a
separate Office workflow; local booking does not require an SMTP or Zoho
Campaigns connection.

## Verify with a local test

Use a disposable meeting type and a link limited to one use. Open the link in a
browser on the same machine, confirm the displayed time zone and slots, and
submit a test guest using a reserved example address such as
`guest@example.test`. Check that one booking appears in **Meetings**, then use
the manage link to reschedule and cancel it. No external email or calendar
invitation is needed for this local flow.

Do not copy real booking URLs, management URLs, IDs, account credentials, or
private guest data into source control, public docs, or issue reports. Local
loopback links are useful only to browsers on the Office host. To let external
guests use links in a deployment, configure a real HTTPS origin such as
`https://office.specify.app`, route it through TLS and the reverse proxy, and
create fresh links after changing the origin. This does not require exposing a
development machine through a temporary tunnel.

## Troubleshooting

- **No calendar is available when creating a type:** return to **Settings →
  Calendars**, finish internal calendar setup, then reload the meeting-type
  form.
- **No slots appear:** check the host's weekly windows, IANA time zone, blocked
  exceptions, duration, buffers, minimum notice, booking horizon, and that the
  meeting type is active.
- **The link opens on the wrong port or host:** verify the API/worker
  `PUBLIC_BASE_URL` and the workspace override. `PUBLIC_BASE_URL` is separate
  from Next.js `API_ORIGIN`; changing only `API_ORIGIN` will not change generated
  booking URLs.
- **Admin pages fail but the public link opens:** confirm that Next.js
  `API_ORIGIN` matches `API_ADDR`, and that the Rust API is running.
- **A guest's slot was taken:** refresh the public page and choose an available
  time. Slot results can become stale between viewing and submission.
- **A remote guest cannot open a link using `127.0.0.1`:** that address points
  back to the guest's computer. Deploy to an HTTPS hostname before sharing with
  people outside the Office host.

## Local verification evidence

On 2026-10-10, a controlled SMTP test delivered an Office template containing
both a booking button and an unsubscribe link pointing to the local frontend
at `http://127.0.0.1:13000`. The delivered HTML and text matched the frozen
Office message. Opening the received booking button and submitting the guest
form produced a confirmed booking and one persistent `office_calendar` event.
A separate link requesting the same host/time was rejected with
`409 SLOT_UNAVAILABLE`. Rescheduling updated the event and the management
screen; cancellation released all associated holds and left no active event.

The received unsubscribe link opened Office's frontend. GET made no change;
explicit confirmation saved one Rust-backed suppression and one CRM activity,
and repeating the request created no duplicate. No public tunnel or external
calendar was used. The test booking was cancelled, bounded test consent
revoked, and the explicitly authorized simulated test reviewer disabled. The
simulated reviewer does not establish independent human review. Private
recipients, identifiers and capability URLs are not included in this evidence.
