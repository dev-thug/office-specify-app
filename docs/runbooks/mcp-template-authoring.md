# Author email templates through MCP

Agents can create a reusable template container, add immutable content
versions, and render a preview through MCP. Template authoring is preparation
work: it does not create a message, grant approval, or send email.

## Required scopes

| Action | Required access |
| --- | --- |
| Create a container or content version | `templates.write` |
| Read template metadata or versions | `templates.read` |
| Render a preview as an agent | `templates.read` and `mail.compose` |

The preview uses safe placeholder URLs for system-owned booking and unsubscribe
links. A preview is not a real recipient message and does not mint capabilities.

## Create and decorate a template

1. Call `templates_create` with a name and a stable idempotency key. Use the
   returned `template_id` for the first content version:

   ```json
   {
     "idempotency_key": "template-container-sales-followup-001",
     "name": "Sales follow-up"
   }
   ```

2. Call `templates_create_draft_version` with that ID, an email subject, plain
   text, optional HTML, and a closed variable schema. Placeholders use the
   literal `{{ variable_name }}` form. The application validates the subject,
   schema, and every placeholder before storing the version.

   ```json
   {
     "template_id": "<template_id returned by templates_create>",
     "idempotency_key": "template-version-sales-followup-001",
     "subject": "A quick question for {{ first_name }} at {{ company_name }}",
     "body_text": "Hi {{ first_name }},\n\nWould a short conversation help? {{ meeting.booking_url }}\n\nUnsubscribe: {{ compliance.unsubscribe_url }}",
     "body_html": "<table style=\"width:100%;max-width:600px;border-collapse:collapse\"><tr><td style=\"background-color:#f3f4f6;padding:24px\"><p>Hi {{ first_name }},</p><p>Would a short conversation help?</p><a href=\"{{ meeting.booking_url }}\" style=\"display:inline-block;background-color:#171717;color:#ffffff;font-size:16px;font-weight:600;padding:12px 24px;border-radius:4px;text-decoration:none\">Choose a time</a><p><a href=\"{{ compliance.unsubscribe_url }}\">Unsubscribe</a></p></td></tr></table>",
     "variable_schema": {
       "type": "object",
       "properties": {
         "first_name": {"type": "string", "maxLength": 80},
         "company_name": {"type": "string", "maxLength": 160},
         "meeting.booking_url": {"type": "string", "format": "uri"},
         "compliance.unsubscribe_url": {"type": "string", "format": "uri"}
       },
       "required": [
         "first_name",
         "company_name",
         "meeting.booking_url",
         "compliance.unsubscribe_url"
       ],
       "additionalProperties": false
     }
   }
   ```

   The schema supports string properties, optional positive `maxLength`,
   optional `format: "uri"`, string defaults, and a `required` list. It accepts
   no other JSON Schema keywords. Every placeholder in the subject, text, or
   HTML must be declared. Required system variables may appear in `properties`
   and `required`, but they must not have defaults.

3. Use the returned `template_version_id` to call
   `templates_render_version`, supplying only sample values for ordinary
   variables:

   ```json
   {
     "template_version_id": "<template_version_id returned above>",
     "variables": {
       "first_name": "Ari",
       "company_name": "Northwind"
     }
   }
   ```

   The result shows the rendered subject, text and HTML. The server injects
   deterministic `preview.invalid` values for `meeting.booking_url` and
   `compliance.unsubscribe_url`; never pass those keys in `variables`.

4. Inspect the saved version with `templates_get_version`, or list versions
   with `templates_list_versions`. Versions are immutable. To change content,
   create another version under the same template container with a new
   idempotency key, then preview and review that version.

## HTML and variable safety

The HTML check uses an HTML5 parser and rejects unsafe or unsupported
constructs. Email formatting may use paragraphs, headings, lists, links,
emphasis, and tables. A limited inline style allowlist supports text and CTA
decoration: `color`, `background-color`, `font-size`, `font-weight`,
`font-family`, `text-align`, `line-height`, `padding`, `margin`, `border`,
`border-radius`, `width`, `max-width`, `text-decoration`, `display`,
`border-collapse`, `border-spacing`, `vertical-align`, and `table-layout`.
Values must use the documented safe forms, such as hex or named colors,
bounded `rgb()`/`rgba()` colors, bounded sizes, common system font families,
font weights from 100 to 900 (or standard weight keywords), nonnegative
spacing, and `display:inline`, `inline-block`, or `block`. Rendering normalizes
validated HTML and keeps only permitted tags, attributes, and styles.
Font sizes must be 8–72px or 6–54pt; relative font sizes are rejected.
Foreground text colors must be at least 80% opaque so CSS cannot make the
message nearly invisible. This is a visibility guard, not a contrast checker.

Scriptable or embedded content, inline event handlers, images, frames, forms,
style blocks, `url()`, `expression()`, `@import`, CSS comments and escapes,
unknown properties, `position`, `behavior`, `display:none`, and `javascript:`
URLs are rejected. CSS functions are disallowed except bounded `rgb()` and
`rgba()` colors. Template values are HTML-escaped when inserted into HTML.

Subjects cannot contain carriage returns or line feeds. Variable names are
case-sensitive identifiers; dotted system-variable names are single names,
not nested object paths. There are no expressions, functions, loops, or file
access. A missing required preview value is an error. Values for system-owned
variables are always supplied by Office and cannot be overridden.

Use a stable ASCII `idempotency_key` of 16 to 128 characters for each create
operation. Non-ASCII keys are rejected; the shared idempotency store measures
key length in bytes. Retry an unchanged request with the same key; use a new
key for a new container or version. Read the MCP error envelope on failure.
Common validation errors include undeclared placeholders, unsupported schema
keys, unsafe HTML, header line breaks, and missing required render values.

## Approval and sending remain separate

Creating a template container or version never creates a message or
authorization to approve or send. Agents still cannot approve or directly send
mail. A later workflow must select the intended version for a message draft,
and any approval or delivery action remains on its separately authorized
human-controlled path.
