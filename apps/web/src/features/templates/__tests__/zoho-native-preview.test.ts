import { describe, expect, it } from "vitest";

import { createZohoNativePreviewDocument } from "../zoho-native-preview";

describe("Zoho native HTML preview", () => {
  it("preserves inline layout and style while removing active content and network loads", () => {
    const source = `<!doctype html>
      <html><head>
        <style>.layout { color: teal; } .hero { background-image: url(https://images.example.test/hero.png); }</style>
        <meta http-equiv="refresh" content="0;url=https://evil.example.test/">
        <base href="https://evil.example.test/">
      </head><body>
        <table class="layout"><tr><td style="padding: 12px">Welcome</td></tr></table>
        <img src="https://track.example.test/pixel.png" onerror="run()" alt="logo">
        <script>run()</script><form action="https://evil.example.test"><input value="x"></form>
        <iframe src="https://evil.example.test"></iframe><object data="https://evil.example.test"></object><embed src="https://evil.example.test">
      </body></html>`;

    const document = createZohoNativePreviewDocument(source);

    expect(document).toContain("default-src 'none'");
    expect(document).toContain(".layout { color: teal; }");
    expect(document).toContain("<table");
    expect(document).toContain("padding: 12px");
    expect(document).toContain("Welcome");
    expect(document).not.toContain("<script");
    expect(document.match(/<meta/g)).toHaveLength(1);
    expect(document).not.toContain("url=https://evil.example.test");
    expect(document).not.toContain("<base");
    expect(document).not.toContain("<form");
    expect(document).not.toContain("<iframe");
    expect(document).not.toContain("<object");
    expect(document).not.toContain("<embed");
    expect(document).not.toContain("onerror");
    expect(document).not.toContain("https://images.example.test");
    expect(document).not.toContain("https://track.example.test");
    expect(document).not.toContain("https://evil.example.test");
  });

  it("keeps same-document anchors and removes unsafe schemes from URL attributes", () => {
    const document = createZohoNativePreviewDocument(
      '<p><a href="#details">Details</a><a href="javascript:alert(1)">Unsafe</a><img src="data:image/png;base64,AAAA" alt="inline"></p>'
    );

    expect(document).toContain('href="#details"');
    expect(document).not.toContain("javascript:");
    expect(document).toContain("data:image/png;base64,AAAA");
  });
});
