import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { LoginForm } from "@/app/(auth)/login/login-form";

const mocks = vi.hoisted(() => ({ login: vi.fn(), push: vi.fn(), refresh: vi.fn() }));
vi.mock("next-intl", () => ({ useTranslations: () => (key: string) => key }));
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: mocks.push, refresh: mocks.refresh }),
  useSearchParams: () => new URLSearchParams(),
}));
vi.mock("@/lib/api/auth", () => ({ login: mocks.login }));

describe("authentication cache isolation", () => {
  it("removes the previous user's template and identity cache before a new login", async () => {
    const client = new QueryClient();
    client.setQueryData(["me"], { workspace_id: "previous-workspace", role: "admin" });
    client.setQueryData(["zoho-email-template", "private-template"], { content: "previous private body" });
    mocks.login.mockImplementation(async () => {
      expect(client.getQueryData(["me"])).toBeUndefined();
      expect(client.getQueryData(["zoho-email-template", "private-template"])).toBeUndefined();
      return { data: {} };
    });
    render(<QueryClientProvider client={client}><LoginForm /></QueryClientProvider>);
    await waitFor(() => expect(screen.getByRole("button", { name: "submit" })).toBeEnabled());
    fireEvent.change(screen.getByLabelText("email"), { target: { value: "new@example.test" } });
    fireEvent.change(screen.getByLabelText("password"), { target: { value: "synthetic-password" } });
    fireEvent.click(screen.getByRole("button", { name: "submit" }));
    await waitFor(() => expect(mocks.push).toHaveBeenCalledWith("/app/dashboard"));
  });
});
