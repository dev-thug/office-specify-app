import { afterEach, describe, expect, it, vi } from "vitest";

import {
  commitImport,
  createExport,
  createImport,
  createUploadRequest,
  importCommitPath,
  importJobId,
  importRowsPath,
  uploadObject,
  type UploadRequestData,
} from "@/lib/api/imports";

function mockFetch(status = 200, body: unknown = { data: {} }) {
  const calls: { url: string; init: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init: init ?? {} });
      return new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      });
    })
  );
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("imports api", () => {
  it("requests an upload URL with an idempotency key", async () => {
    const calls = mockFetch(200, {
      data: { storage_key: "k1", upload_url: "/api/v1/imports/uploads/k1", upload_token: "cap", max_bytes: 42, expires_at: "2026-10-10T00:00:00Z" },
    });
    await createUploadRequest(
      { filename: "a.csv", media_type: "text/csv", byte_size: 42 },
      { idempotencyKey: "up-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/imports/upload-requests");
    expect(h.get("Idempotency-Key")).toBe("up-key");
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      filename: "a.csv",
      media_type: "text/csv",
      byte_size: 42,
    });
  });

  it("uploads raw bytes to the app-origin upload route with the capability token", async () => {
    const calls = mockFetch(201, { data: { object_id: "o1", storage_key: "k1", media_type: "text/csv", byte_size: 3, content_hash: "h1" } });
    const req: UploadRequestData = {
      storage_key: "k1",
      upload_url: "/api/v1/imports/uploads/k1",
      upload_token: "cap",
      max_bytes: 42,
    };
    const file = new Blob(["a,b"], { type: "text/csv" });
    const stored = await uploadObject(req, file);
    expect(stored.object_id).toBe("o1");
    expect(calls[0]!.url).toBe("/api/v1/imports/uploads/k1?token=cap");
    expect(calls[0]!.init.method).toBe("PUT");
    expect(calls[0]!.init.body).toBe(file);
  });

  it("rejects a failed object upload", async () => {
    mockFetch(403, null);
    await expect(
      uploadObject(
        { storage_key: "k1", upload_url: "/api/v1/imports/uploads/k1", upload_token: "cap", max_bytes: 42 },
        new Blob(["x"])
      )
    ).rejects.toThrow();
  });

  it("creates a validate job with stored_object_id and mapping", async () => {
    const calls = mockFetch(202, { data: { id: "i1" } });
    await createImport(
      {
        stored_object_id: "o1",
        mapping: { columns: { 회사명: "company.name" }, has_header: true, duplicate_strategy: "review" },
      },
      { idempotencyKey: "imp-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/imports");
    expect(h.get("Idempotency-Key")).toBe("imp-key");
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      stored_object_id: "o1",
      mapping: { columns: { 회사명: "company.name" }, has_header: true, duplicate_strategy: "review" },
    });
  });

  it("commits with validation_hash and invalid_row_policy", async () => {
    const calls = mockFetch(202, { data: { id: "i1" } });
    await commitImport(
      "i1",
      { validation_hash: "ab12", invalid_row_policy: "skip" },
      { idempotencyKey: "commit-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/imports/i1/commit");
    expect(h.get("If-Match")).toBeNull();
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({
      validation_hash: "ab12",
      invalid_row_policy: "skip",
    });
  });

  it("creates an export job with an idempotency key", async () => {
    const calls = mockFetch(202, { data: { job_id: "j9" } });
    await createExport(
      { fields: ["company.name"] },
      { idempotencyKey: "exp-key" }
    );
    const h = new Headers(calls[0]!.init.headers);
    expect(calls[0]!.url).toBe("/api/v1/exports");
    expect(JSON.parse(calls[0]!.init.body as string)).toEqual({ fields: ["company.name"] });
    expect(h.get("Idempotency-Key")).toBe("exp-key");
  });

  it("builds paths", () => {
    expect(importRowsPath("i2")).toBe("/api/v1/imports/i2/rows");
    expect(importCommitPath("i2")).toBe("/api/v1/imports/i2/commit");
  });

  it("resolves the resource id from B.3.13's canonical import_id", () => {
    expect(importJobId({ import_id: "b1" })).toBe("b1");
    // 일부 응답이 id 만 주는 경우도 같은 참조로 해석한다.
    expect(importJobId({ import_id: "b1", id: "legacy" })).toBe("b1");
    expect(importJobId({ id: "legacy" })).toBe("legacy");
  });
});
