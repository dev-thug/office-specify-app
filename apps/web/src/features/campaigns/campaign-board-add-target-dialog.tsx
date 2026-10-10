"use client";

import { useQuery } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useMemo, useRef, useState, type FormEvent } from "react";

import { CompanyPicker } from "@/features/companies/company-picker";
import type { Company } from "@/lib/api/companies";
import {
  contactListParams,
  contactsPath,
  getContact,
  isSuppressed,
  type Contact,
} from "@/lib/api/contacts";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { addSalesCampaignTarget } from "@/lib/api/sales-campaigns";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export function CampaignBoardAddTargetDialog({
  campaignId,
  open,
  onOpenChange,
  onAdded,
}: {
  campaignId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onAdded: () => void;
}) {
  const t = useTranslations("campaigns.personalized.target");
  const common = useTranslations("common");
  const [company, setCompany] = useState<Company | null>(null);
  const [contactId, setContactId] = useState("");
  const [emailAddressId, setEmailAddressId] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const operationKey = useRef<string | null>(null);

  const contactsQuery = useCursorList<Contact>({
    queryKey: ["contacts", "sales-campaign-target", campaignId, company?.id],
    path: contactsPath(),
    params: contactListParams({
      company_id: company?.id,
      status: company ? "active" : undefined,
    }),
    limit: 100,
    enabled: open && Boolean(company),
  });
  const contacts = contactsQuery.data?.pages.flatMap((page) => page.data) ?? [];
  const contactQuery = useQuery({
    queryKey: ["contacts", "sales-campaign-target-detail", contactId],
    enabled: open && Boolean(contactId),
    queryFn: async () => (await getContact(contactId)).data,
    retry: false,
  });
  const eligibleEmails = useMemo(() => {
    const emails = contactQuery.data?.emails ?? [];
    return emails.filter(
      (email) =>
        email.verification_status === "verified" &&
        email.marketing_eligibility?.eligible !== false &&
        !isSuppressed(email)
    );
  }, [contactQuery.data]);

  function reset() {
    setCompany(null);
    setContactId("");
    setEmailAddressId("");
    setPending(false);
    setError(null);
    operationKey.current = null;
  }

  function changeCompany(value: Company | null) {
    setCompany(value);
    setContactId("");
    setEmailAddressId("");
    setError(null);
  }

  function changeContact(id: string) {
    setContactId(id === "none" ? "" : id);
    setEmailAddressId("");
    setError(null);
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!company || !contactId || !emailAddressId || pending) return;
    operationKey.current ??= idempotencyKey();
    setPending(true);
    setError(null);
    try {
      await addSalesCampaignTarget(
        campaignId,
        {
          company_id: company.id,
          contact_id: contactId,
          email_address_id: emailAddressId,
        },
        { idempotencyKey: operationKey.current }
      );
      operationKey.current = null;
      reset();
      onOpenChange(false);
      onAdded();
    } catch (cause) {
      if (
        cause instanceof ApiError &&
        cause.problem.retryable !== true &&
        cause.status < 500
      ) {
        operationKey.current = null;
      }
      setError(t("addError"));
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset();
        onOpenChange(next);
      }}
    >
      <DialogContent className="max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("addTitle")}</DialogTitle>
          <DialogDescription>{t("addDescription")}</DialogDescription>
        </DialogHeader>
        <form className="flex flex-col gap-4" onSubmit={(event) => void submit(event)}>
          <div className="grid gap-1.5">
            <Label>{t("company")}</Label>
            <CompanyPicker
              value={company}
              onChange={changeCompany}
              placeholder={t("companyPlaceholder")}
              noneLabel={t("clearCompany")}
              emptyLabel={t("noCompanyMatches")}
              disabled={pending}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="sales-target-contact">{t("contact")}</Label>
            <Select
              value={contactId || "none"}
              onValueChange={changeContact}
              disabled={!company || contactsQuery.isPending || pending}
            >
              <SelectTrigger id="sales-target-contact" aria-label={t("contact")} className="w-full">
                <SelectValue placeholder={t("contactPlaceholder")} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">{t("contactPlaceholder")}</SelectItem>
                {contacts.map((contact) => (
                  <SelectItem key={contact.id} value={contact.id}>
                    {contact.name}
                    {contact.job_title ? ` · ${contact.job_title}` : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {contactsQuery.isPending && company ? (
              <p role="status" className="text-2xs text-muted-foreground">
                {t("loadingContacts")}
              </p>
            ) : null}
            {contactsQuery.isError ? (
              <p role="alert" className="text-2xs text-destructive">{t("contactLoadError")}</p>
            ) : null}
            {!contactsQuery.isPending && !contactsQuery.isError && company && contacts.length === 0 ? (
              <p className="text-2xs text-muted-foreground">{t("noContacts")}</p>
            ) : null}
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="sales-target-email">{t("email")}</Label>
            <Select
              value={emailAddressId || "none"}
              onValueChange={(id) => setEmailAddressId(id === "none" ? "" : id)}
              disabled={!contactId || contactQuery.isPending || pending}
            >
              <SelectTrigger id="sales-target-email" aria-label={t("email")} className="w-full">
                <SelectValue placeholder={t("emailPlaceholder")} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">{t("emailPlaceholder")}</SelectItem>
                {eligibleEmails.map((email) => (
                  <SelectItem key={email.email_address_id} value={email.email_address_id}>
                    {email.address}{email.is_primary ? ` · ${t("primaryEmail")}` : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {contactQuery.isPending && contactId ? (
              <p role="status" className="text-2xs text-muted-foreground">
                {t("loadingContact")}
              </p>
            ) : null}
            {contactQuery.isError ? (
              <p role="alert" className="text-2xs text-destructive">{t("contactLoadError")}</p>
            ) : null}
            {!contactQuery.isPending && !contactQuery.isError && contactId && eligibleEmails.length === 0 ? (
              <p className="text-2xs text-muted-foreground">{t("noEmails")}</p>
            ) : null}
          </div>
          {error ? (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          ) : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={pending}
              onClick={() => onOpenChange(false)}
            >
              {common("cancel")}
            </Button>
            <Button
              type="submit"
              disabled={!company || !contactId || !emailAddressId || pending}
            >
              {pending ? t("adding") : t("add")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
