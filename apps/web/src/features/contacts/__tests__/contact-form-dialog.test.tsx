import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { NextIntlClientProvider } from "next-intl";
import { afterEach, expect, it, vi } from "vitest";
import { ContactFormDialog } from "../contact-form-dialog";
import contacts from "../../../../messages/en/contacts.json";
import common from "../../../../messages/en/common.json";

vi.mock("@/features/companies/company-picker", () => ({ CompanyPicker: () => null }));
vi.mock("@/features/companies/owner-select", () => ({ OwnerSelect: () => null }));
afterEach(() => vi.unstubAllGlobals());

it("creates contact, email and phone in a single request", async () => {
  const fetcher = vi.fn(async () => new Response(JSON.stringify({data:{id:"c1",name:"Alex",phone:"+82 10-1234-5678",emails:[{address:"alex@example.test",is_primary:true}]}}), {status:201,headers:{"content-type":"application/json"}}));
  vi.stubGlobal("fetch", fetcher);
  const saved = vi.fn();
  render(<NextIntlClientProvider locale="en" messages={{contacts,common}}><ContactFormDialog open onOpenChange={vi.fn()} onSaved={saved} /></NextIntlClientProvider>);
  fireEvent.change(screen.getByLabelText("Name"), {target:{value:"Alex"}});
  fireEvent.change(screen.getByLabelText("Email"), {target:{value:"alex@example.test"}});
  fireEvent.change(screen.getByLabelText("Phone"), {target:{value:"+82 10-1234-5678"}});
  fireEvent.click(screen.getByRole("button", {name:contacts.form.submit}));
  await waitFor(() => expect(saved).toHaveBeenCalled());
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(JSON.parse(String((fetcher.mock.calls[0] as unknown as [string,RequestInit])[1].body))).toMatchObject({name:"Alex",email:"alex@example.test",phone:"+82 10-1234-5678"});
});
