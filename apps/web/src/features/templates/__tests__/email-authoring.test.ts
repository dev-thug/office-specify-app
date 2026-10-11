import { describe, expect, it } from "vitest";
import { cleanEmailHtml, emailHtmlToText, emailPreviewDocument, renderSampleEmail, DEFAULT_EMAIL_VARIABLES } from "../email-authoring";

describe("email authoring", () => {
  it("preserves supported formatting, links and variable placeholders", () => {
    const html = '<p>Hello <strong>{{contact.name}}</strong></p><ul><li>One</li></ul><a href="https://example.test">Visit</a>';
    expect(cleanEmailHtml(html)).toEqual({html, removed: false});
  });
  it("drops executable markup, remote resources, handlers and unsafe URLs before display", () => {
    const result = cleanEmailHtml('<script>alert(1)</script><img src="https://tracker.test/pixel"><p onclick="bad()" style="color:red">Safe</p><a href="javascript:bad()">Link</a><svg onload="bad()"></svg>');
    expect(result.removed).toBe(true);
    expect(result.html).toBe('<p style="color:red">Safe</p><a>Link</a>');
  });
  it("extracts readable fallback text with paragraph and list boundaries", () => {
    expect(emailHtmlToText('<p>Hello <b>{{contact.name}}</b></p><p>Next<br>line</p><ul><li>First</li><li>Second</li></ul>')).toBe('Hello {{contact.name}}\n\nNext\nline\n\n• First\n• Second');
  });
  it("retains link destinations in plain-text mail", () => {
    expect(emailHtmlToText('<p><a href="{{meeting.booking_url}}">Book a meeting</a></p>')).toBe('Book a meeting ({{meeting.booking_url}})');
  });
  it("escapes recipient samples and never turns samples into active HTML", () => {
    expect(renderSampleEmail('<p>{{contact.name}}</p>', {'contact.name':'<img src=x onerror=bad()>'})).toBe('<p>&lt;img src=x onerror=bad()&gt;</p>');
  });
  it("provides six distinct presets (incl. the per-company message.body); generated links have no send-time defaults", () => {
    expect(new Set(DEFAULT_EMAIL_VARIABLES.map(v => v.name)).size).toBe(6);
    expect(DEFAULT_EMAIL_VARIABLES.find(v => v.name === "message.body")).toMatchObject({ system: false, type: "string", maxLength: "5000" });
    expect(DEFAULT_EMAIL_VARIABLES.filter(v => v.system).map(v => v.name)).toEqual(['meeting.booking_url', 'compliance.unsubscribe_url']);
    expect(DEFAULT_EMAIL_VARIABLES.every(v => !("default" in v))).toBe(true);
  });
  it("preserves the approved presentation table attributes for the booking CTA", () => {
    const html = '<table role="presentation" border="0" cellpadding="12" cellspacing="0"><tbody><tr><td bgcolor="#f3f4f6"><a href="{{meeting.booking_url}}"><strong>미팅 예약</strong></a></td></tr></tbody></table>';
    expect(cleanEmailHtml(html)).toEqual({html, removed:false});
    expect(emailHtmlToText(html)).toBe('미팅 예약 ({{meeting.booking_url}})');
  });
  it("preserves the bounded backend-approved inline styles through preview rendering", () => {
    const styled = '<table role="presentation" style="width:100%;max-width:600px;margin:0 auto;border-collapse:collapse"><tbody><tr><td style="background-color:#f3f4f6;padding:24px"><h2 style="color:#6d28d9;font-family:Arial,sans-serif;font-size:24px">Northstar</h2><a href="https://example.test" style="display:inline-block;background-color:#f3f4f6;padding:12px 24px;font-weight:600;text-align:center;line-height:1.5;text-decoration:underline">Open</a></td></tr></tbody></table>';
    const cleaned = cleanEmailHtml(styled);

    expect(cleaned).toEqual({ html: styled, removed: false });
    expect(renderSampleEmail(cleaned.html, {})).toBe(styled);
    const preview = emailPreviewDocument(cleaned.html);
    expect(preview).toContain('style="width:100%;max-width:600px');
    expect(preview).toContain('style="color:#6d28d9;font-family:Arial,sans-serif;font-size:24px"');
    expect(preview).toContain('style="display:inline-block;background-color:#f3f4f6');
    expect(preview).not.toContain('href="https://example.test"');
  });

  it("rejects dangerous, hiding, out-of-range, and unsupported inline CSS", () => {
    for (const style of [
      "position:fixed",
      "display:none",
      "opacity:0",
      "font-size:1px",
      "font-weight:600.0",
      "line-height:0",
      "color:transparent",
      "background-image:url(https://evil.test/pixel)",
      "color:expression(alert(1))",
      "color:red!important",
      "color:red/**/",
      "margin:-100px",
      "font-family:Arial/evil",
      "color:red;color:blue",
    ]) {
      expect(cleanEmailHtml(`<p style="${style}">Keep safe text</p>`)).toEqual({
        html: "<p>Keep safe text</p>",
        removed: true,
      });
    }
  });

  it("strips table attributes outside the strict presentation whitelist", () => {
    const raw = '<table role="grid" border="2" cellpadding="33" cellspacing="17" background="https://evil.test/pixel" onclick="bad()" style="color:red"><tbody><tr><td bgcolor="red" onmouseover="bad()" style="color:red"><a href="{{meeting.booking_url}}">Book</a></td></tr></tbody></table>';
    expect(cleanEmailHtml(raw)).toEqual({
      html:'<table style="color:red"><tbody><tr><td style="color:red"><a href="{{meeting.booking_url}}">Book</a></td></tr></tbody></table>',
      removed:true,
    });
  });
  it("keeps line breaks of a substituted multi-line value in the sample preview, still escaped", () => {
    const html = renderSampleEmail("<p>{{message.body}}</p>", { "message.body": "첫 줄 <b>\n둘째 줄" });
    expect(html).toContain("첫 줄 &lt;b&gt;<br>둘째 줄");
  });
});
