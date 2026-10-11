"use client";

import Link from "next/link";
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
    }),
    limit: 100,
    enabled: open && Boolean(company),
  });
  const contacts = (contactsQuery.data?.pages.flatMap((page) => page.data) ?? []).filter(contact => !contact.archived_at && ["new", "active"].includes(contact.status));
  const contactQuery = useQuery({
    queryKey: ["contacts", "sales-campaign-target-detail", contactId],
    enabled: open && Boolean(contactId),
    queryFn: async () => (await getContact(contactId)).data,
    retry: false,
  });
  const eligibleEmails = useMemo(() => {
    const emails = contactQuery.data?.emails ?? [];
    return emails;
  }, [contactQuery.data]);

  // 담당자를 고르면 수신 이메일은 대표(없으면 첫 번째) 주소로 자동 선택한다.
  // 주소가 여러 개일 때만 선택 상자를 보여 준다.
  const defaultEmail = useMemo(
    () =>
      eligibleEmails.find((email) => email.is_primary && !isSuppressed(email)) ??
      eligibleEmails.find((email) => !isSuppressed(email)) ??
      eligibleEmails.find((email) => email.is_primary) ??
      eligibleEmails[0],
    [eligibleEmails]
  );
  const effectiveEmailId =
    eligibleEmails.find((email) => email.email_address_id === emailAddressId)
      ?.email_address_id ??
    defaultEmail?.email_address_id ??
    "";
  const selectedEmail = eligibleEmails.find(
    (email) => email.email_address_id === effectiveEmailId
  );
  // 발송 근거는 참고 정보이며 차단 조건이 아니다. 수신 거부 주소만 경고한다.
  const emailSuppressed = selectedEmail ? isSuppressed(selectedEmail) : false;

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
    if (!company || !contactId || !effectiveEmailId || pending) return;
    operationKey.current ??= idempotencyKey();
    setPending(true);
    setError(null);
    try {
      await addSalesCampaignTarget(
        campaignId,
        {
          company_id: company.id,
          contact_id: contactId,
          email_address_id: effectiveEmailId,
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
                    {contact.emails?.length ? ` · ${(contact.emails.find(email => email.is_primary) ?? contact.emails[0])?.address}` : ""}
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
            {contactsQuery.hasNextPage ? <Button type="button" size="sm" variant="ghost" disabled={contactsQuery.isFetchingNextPage} onClick={() => void contactsQuery.fetchNextPage()}>{common("table.loadMore")}</Button> : null}
            {company ? <Link href={`/app/companies/${company.id}`} className="text-xs text-primary underline">{t("manageContacts")}</Link> : null}
          </div>
          {contactId && eligibleEmails.length === 1 && selectedEmail ? (
            <div className="grid gap-1.5">
              <Label>{t("email")}</Label>
              <p className="rounded-full border border-input px-4 py-2 text-sm">
                {selectedEmail.address}
              </p>
            </div>
          ) : (
            <div className="grid gap-1.5">
              <Label htmlFor="sales-target-email">{t("email")}</Label>
              <Select
                value={effectiveEmailId || "none"}
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
          )}
          {contactId ? <Link href={`/app/contacts/${contactId}`} className="text-xs text-primary underline">{t("manageEmails")}</Link> : null}
          {emailSuppressed ? <Alert variant="warning"><AlertDescription>{t("suppressedEmail")}</AlertDescription></Alert> : null}
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
              disabled={!company || !contactId || !effectiveEmailId || pending}
            >
              {pending ? t("adding") : t("add")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
