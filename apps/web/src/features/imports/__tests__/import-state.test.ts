import { describe, expect, it } from "vitest";

import {
  canCommitImport,
  csvCell,
  failedRowsCsv,
  guessTargetField,
  isActiveImportStatus,
  parseCsvSample,
  rowsToCsv,
} from "@/features/imports";
import type { ImportRow } from "@/lib/api/imports";

describe("import status rules", () => {
  it("marks uploaded/validating/committing as active", () => {
    for (const s of ["uploaded", "validating", "committing"]) {
      expect(isActiveImportStatus(s)).toBe(true);
    }
    for (const s of ["ready", "completed", "failed", "canceled"]) {
      expect(isActiveImportStatus(s)).toBe(false);
    }
  });

  it("allows commit only when ready with a validation hash", () => {
    expect(
      canCommitImport({ status: "ready", validation_hash: "ab" })
    ).toBe(true);
    expect(canCommitImport({ status: "ready", validation_hash: null })).toBe(
      false
    );
    expect(
      canCommitImport({ status: "validating", validation_hash: "ab" })
    ).toBe(false);
    expect(
      canCommitImport({ status: "completed", validation_hash: "ab" })
    ).toBe(false);
  });
});

describe("parseCsvSample", () => {
  it("parses headers and rows with quoting and commas", () => {
    const csv = 'name,email,note\n"ACME, Inc",a@b.co,"say ""hi"""\nBeta,c@d.io,x\n';
    const { headers, rows } = parseCsvSample(csv, 5);
    expect(headers).toEqual(["name", "email", "note"]);
    expect(rows[0]).toEqual(["ACME, Inc", "a@b.co", 'say "hi"']);
    expect(rows[1]).toEqual(["Beta", "c@d.io", "x"]);
  });

  it("handles CRLF and skips empty lines", () => {
    const { headers, rows } = parseCsvSample("a,b\r\n\r\n1,2\r\n", 5);
    expect(headers).toEqual(["a", "b"]);
    expect(rows).toEqual([["1", "2"]]);
  });

  it("caps sample rows", () => {
    const csv = "h\n1\n2\n3\n4\n5\n6\n";
    expect(parseCsvSample(csv, 3).rows).toHaveLength(3);
  });
});

describe("guessTargetField", () => {
  it("maps common headers", () => {
    expect(guessTargetField("회사명")).toBe("company.name");
    expect(guessTargetField("Company Name")).toBe("company.name");
    expect(guessTargetField("Contact Email")).toBe("contact.email");
    expect(guessTargetField("domain")).toBe("company.domain");
    expect(guessTargetField("직함")).toBe("contact.job_title");
    expect(guessTargetField("random")).toBe("");
  });
});

describe("csv export", () => {
  it("escapes commas, quotes and newlines", () => {
    expect(csvCell('a,b')).toBe('"a,b"');
    expect(csvCell('say "hi"')).toBe('"say ""hi"""');
    expect(csvCell("x\ny")).toBe('"x\ny"');
    expect(csvCell("plain")).toBe("plain");
  });

  it("prefixes formula-injection leading characters", () => {
    for (const bad of ["=1+1", "+123", "-5", "@x", "\t=x"]) {
      expect(csvCell(bad).startsWith("'")).toBe(true);
    }
  });

  it("serializes failed rows with status and errors columns", () => {
    const rows: ImportRow[] = [
      {
        row_no: 3,
        normalized_data: { name: "ACME", email: "bad@" },
        status: "invalid",
        errors: [{ field: "email", code: "FORMAT" }],
        company_id: null,
        contact_id: null,
      },
    ];
    const csv = failedRowsCsv(rows);
    const lines = csv.split("\r\n").filter(Boolean);
    expect(lines[0]).toBe("name,email,__status,__errors");
    expect(lines[1]).toContain("invalid");
    expect(lines[1]).toContain("email: FORMAT");
    expect(rowsToCsv([["a"], ["b"]])).toBe("a\r\nb\r\n");
  });
});
