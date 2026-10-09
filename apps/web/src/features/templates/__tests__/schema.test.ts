import { describe, expect, it } from "vitest";

import {
  buildVariableSchema,
  variablesOutsideSchema,
  type VariableRow,
} from "@/features/templates/version-editor";

const row = (partial: Partial<VariableRow>): VariableRow => ({
  key: "k",
  name: "",
  type: "string",
  required: false,
  maxLength: "",
  ...partial,
});

describe("buildVariableSchema", () => {
  it("행 목록을 JSON Schema object로 변환한다 (additionalProperties: false)", () => {
    const schema = buildVariableSchema([
      row({ name: "company.name", required: true, maxLength: "300" }),
      row({ name: "meeting.booking_url", type: "uri" }),
    ]);
    expect(schema.type).toBe("object");
    expect(schema.additionalProperties).toBe(false);
    expect(schema.required).toEqual(["company.name"]);
    expect(schema.properties["company.name"]).toEqual({
      type: "string",
      maxLength: 300,
    });
    // uri 타입은 type:"string" + format:"uri"로 직렬화한다 (B.3.5).
    expect(schema.properties["meeting.booking_url"]).toEqual({
      type: "string",
      format: "uri",
    });
  });

  it("빈 이름 행·잘못된 maxLength는 건너뛴다", () => {
    const schema = buildVariableSchema([
      row({ name: "  " }),
      row({ name: "a", maxLength: "abc" }),
    ]);
    expect(schema.properties).toEqual({ a: { type: "string" } });
    expect(schema.required).toEqual([]);
  });
});

describe("variablesOutsideSchema", () => {
  it("본문 {{placeholder}} 중 스키마에 없는 이름을 찾는다", () => {
    const rows = [row({ name: "contact.name" })];
    expect(
      variablesOutsideSchema(
        rows,
        "안녕하세요 {{contact.name}}님 — {{meeting.booking_url}}",
        null
      )
    ).toEqual(["meeting.booking_url"]);
  });

  it("스키마에 있는 변수·공백 허용 자리표시자는 제외한다", () => {
    const rows = [row({ name: "company.name" })];
    expect(
      variablesOutsideSchema(rows, "{{ company.name }}", "본문 없음")
    ).toEqual([]);
  });
});
