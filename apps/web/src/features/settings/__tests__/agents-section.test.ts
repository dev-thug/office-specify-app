import { describe, expect, it } from "vitest";

import { AGENT_SCOPES } from "../agents-section";

describe("agent credential scopes", () => {
  it("offers every scope a human operator holds, including direct send and approval", () => {
    expect(AGENT_SCOPES).toEqual(expect.arrayContaining([
      "crm.write",
      "crm.import",
      "crm.export",
      "mail.manage",
      "mail.assign",
      "mail.submit_approved",
      "mail.send",
      "mail.approve",
      "campaign.submit_approved",
      "org.admin",
    ]));
  });
});
