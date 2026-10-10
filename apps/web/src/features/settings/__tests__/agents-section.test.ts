import { describe, expect, it } from "vitest";

import { AGENT_SCOPES } from "../agents-section";

describe("agent credential scopes", () => {
  it("offers mailbox management and human-approved submission without direct send or approval", () => {
    expect(AGENT_SCOPES).toEqual(expect.arrayContaining([
      "mail.manage",
      "mail.assign",
      "mail.submit_approved",
      "campaign.submit_approved",
    ]));
    expect(AGENT_SCOPES).not.toContain("mail.send");
    expect(AGENT_SCOPES).not.toContain("mail.approve");
  });
});
