import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import enTemplates from "../../../../messages/en/templates.json";
import { EmailCanvas } from "../email-canvas";

const styledEmail =
  '<table role="presentation" style="width:100%;max-width:600px;margin:0 auto;border-collapse:collapse"><tbody><tr><td style="background-color:#f3f4f6;padding:24px"><h2 style="color:#6d28d9;font-family:Arial,sans-serif;font-size:24px">Northstar</h2><a href="https://example.test" style="display:inline-block;background-color:#f3f4f6;padding:12px 24px;font-weight:600;text-align:center;line-height:1.5;text-decoration:underline">Open</a></td></tr></tbody></table>';

function renderCanvas() {
  return render(
    <NextIntlClientProvider locale="en" messages={{ templates: enTemplates }}>
      <EmailCanvas
        html={styledEmail}
        previewHtml={styledEmail}
        previewSubject="Northstar follow-up"
        previewButtonLabel="Sample preview"
        onChange={vi.fn()}
        onFocus={() => undefined}
      />
    </NextIntlClientProvider>
  );
}

describe("EmailCanvas inline-style round-trip", () => {
  it("keeps the safe style subset in compose, HTML source, and sample preview modes", async () => {
    renderCanvas();

    const canvas = screen.getByRole("textbox", { name: "Email message" });
    await waitFor(() => expect(canvas.innerHTML).toContain("max-width:600px"));
    expect(canvas.innerHTML).toContain("color:#6d28d9");

    fireEvent.click(screen.getByRole("button", { name: "Edit HTML" }));
    expect(screen.getByRole("textbox", { name: "HTML code" })).toHaveValue(styledEmail);

    fireEvent.click(screen.getByRole("button", { name: "Back to email canvas" }));
    fireEvent.click(screen.getByRole("button", { name: "Sample preview" }));
    expect(
      await screen.findByText("Sample data only · not saved as recipient values")
    ).toBeInTheDocument();
    const preview = await screen.findByTitle("Recipient email preview");
    expect(preview).toHaveAttribute("sandbox", "");
    expect(preview.getAttribute("srcdoc")).toContain("max-width:600px");
    expect(preview.getAttribute("srcdoc")).toContain("color:#6d28d9");
    expect(preview.getAttribute("srcdoc")).not.toContain('href="https://example.test"');
  });
});
