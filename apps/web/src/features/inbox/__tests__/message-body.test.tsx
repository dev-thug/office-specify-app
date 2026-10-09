import { NextIntlClientProvider } from "next-intl";
import { render, screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { describe, expect, it } from "vitest";

import { MessageBody } from "../message-body";
import enInbox from "../../../../messages/en/inbox.json";

const messages = { inbox: enInbox };

const R = (ui: ReactElement) =>
  render(
    <NextIntlClientProvider locale="en" messages={messages}>
      {ui}
    </NextIntlClientProvider>
  );

describe("MessageBody — XSS 방어 (§15.1)", () => {
  it("정제된 HTML 은 권한 없는 sandboxed iframe srcDoc 으로만 렌더한다", () => {
    const { container } = R(
      <MessageBody
        bodyHtmlSanitized="<p>hi<script>alert(1)</script></p>"
        bodyText={null}
        subject="hello"
      />
    );
    const iframe = container.querySelector("iframe");
    expect(iframe).not.toBeNull();
    // sandbox 가 비어 있다 = 스크립트·폼·팝업·same-origin 전부 차단
    expect(iframe).toHaveAttribute("sandbox", "");
    expect(iframe).not.toHaveAttribute("sandbox", expect.stringContaining("allow-scripts"));
    const srcdoc = iframe!.getAttribute("srcdoc") ?? "";
    expect(srcdoc).toContain("Content-Security-Policy");
    expect(srcdoc).toContain("default-src 'none'");
    expect(srcdoc).toContain("<p>hi<script>alert(1)</script></p>");
    // HTML 을 DOM 에 직접 주입하지 않는다
    expect(container.querySelector("script")).toBeNull();
  });

  it("정제 HTML 이 없으면 텍스트로만 렌더하고 iframe 을 만들지 않는다", () => {
    const { container } = R(
      <MessageBody
        bodyHtmlSanitized={null}
        bodyText={"<img src=x onerror=alert(1)>"}
      />
    );
    expect(container.querySelector("iframe")).toBeNull();
    // 텍스트 노드로 렌더 — 태그로 해석되지 않는다
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByText(/onerror/)).toBeInTheDocument();
  });

  it("본문이 없으면 빈 상태 문구를 보인다", () => {
    R(<MessageBody bodyHtmlSanitized={null} bodyText={null} />);
    expect(screen.getByText("No body to display")).toBeInTheDocument();
  });
});
