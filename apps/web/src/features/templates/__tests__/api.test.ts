import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createEmailTemplate,
  createTemplateVersion,
  renderTemplateVersion,
} from "@/lib/api/templates";
import { ApiError } from "@/lib/api/problem";

function mockFetch(
  status = 200,
  body: unknown = { data: {} },
  headers: Record<string, string> = {}
) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json", ...headers },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("templates API", () => {
  it("createEmailTemplate — POST /email-templates + 멱등 키", async () => {
    const calls = mockFetch(201, { data: { template_id: "t1" } });
    const result = await createEmailTemplate({ name: "템플릿" }, { idempotencyKey: "k1" });
    expect(result.data.template_id).toBe("t1");
    expect(calls[0]!.url).toBe("/api/v1/email-templates");
    expect(new Headers(calls[0]!.init.headers).get("Idempotency-Key")).toBe(
      "k1"
    );
    expect(JSON.parse(String(calls[0]!.init.body)).name).toBe("템플릿");
  });

  it("createTemplateVersion — POST /email-templates/{id}/versions (B.3.5 본문)", async () => {
    const calls = mockFetch(201, {
      data: { template_version_id: "v1", version_no: 2 },
    });
    const res = await createTemplateVersion(
      "t1",
      {
        subject: "제목 {{contact.name}}",
        body_text: "본문",
        body_html: null,
        variable_schema: {
          type: "object",
          properties: { "contact.name": { type: "string" } },
          required: ["contact.name"],
          additionalProperties: false,
        },
      },
      { idempotencyKey: "k2" }
    );
    expect(calls[0]!.url).toBe("/api/v1/email-templates/t1/versions");
    const body = JSON.parse(String(calls[0]!.init.body));
    expect(body.variable_schema.required).toEqual(["contact.name"]);
    expect(res.data.version_no).toBe(2);
  });

  it("renderTemplateVersion — POST /template-versions/{id}/render, 멱등 키 없음", async () => {
    const calls = mockFetch(200, {
      data: { subject: "렌더됨", warnings: [] },
    });
    await renderTemplateVersion("v1", {
      variables: { "contact.name": "김" },
    });
    expect(calls[0]!.url).toBe("/api/v1/template-versions/v1/render");
    expect(
      new Headers(calls[0]!.init.headers).get("Idempotency-Key")
    ).toBeNull();
  });

  it("422 TEMPLATE_VARIABLE_MISSING → ApiError.code + violations", async () => {
    mockFetch(
      422,
      {
        type: "urn:outreach:problem:template-variable-missing",
        title: "missing variables",
        status: 422,
        code: "TEMPLATE_VARIABLE_MISSING",
        violations: [{ field: "variables.contact.name", code: "REQUIRED" }],
      },
      { "content-type": "application/problem+json" }
    );
    const err = await renderTemplateVersion("v1", { variables: {} }).catch(
      (e: unknown) => e
    );
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).code).toBe("TEMPLATE_VARIABLE_MISSING");
    expect((err as ApiError).problem.violations[0]?.field).toBe(
      "variables.contact.name"
    );
  });
});
