import { describe, expect, it } from "vitest";
import { cleanEmailHtml, emailHtmlToText, renderSampleEmail, DEFAULT_EMAIL_VARIABLES } from "../email-authoring";

describe("email authoring", () => {
  it("preserves supported formatting, links and variable placeholders", () => {
    const html = '<p>Hello <strong>{{contact.name}}</strong></p><ul><li>One</li></ul><a href="https://example.test">Visit</a>';
    expect(cleanEmailHtml(html)).toEqual({html, removed: false});
  });
  it("drops executable markup, remote resources, handlers and unsafe URLs before display", () => {
    const result = cleanEmailHtml('<script>alert(1)</script><img src="https://tracker.test/pixel"><p onclick="bad()" style="color:red">Safe</p><a href="javascript:bad()">Link</a><svg onload="bad()"></svg>');
    expect(result.removed).toBe(true);
    expect(result.html).toBe('<p>Safe</p><a>Link</a>');
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
  it("provides five distinct presets; generated links have no send-time defaults", () => {
    expect(new Set(DEFAULT_EMAIL_VARIABLES.map(v => v.name)).size).toBe(5);
    expect(DEFAULT_EMAIL_VARIABLES.filter(v => v.system).map(v => v.name)).toEqual(['meeting.booking_url', 'compliance.unsubscribe_url']);
    expect(DEFAULT_EMAIL_VARIABLES.every(v => !("default" in v))).toBe(true);
  });
});
