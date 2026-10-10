"use client";

import Link from "next/link";
import { useRef, useState, type FormEvent } from "react";
import { useTranslations } from "next-intl";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import {
  ContactComposeDialog,
  type ContactComposeRecipient,
} from "@/features/contacts/contact-compose-dialog";
import { ContactFormDialog } from "@/features/contacts/contact-form-dialog";
import { NotesPanel } from "@/features/companies/notes-panel";
import { principalName, useHumanPrincipals } from "@/features/companies/owner-select";
import { TasksPanel } from "@/features/companies/tasks-panel";
import { addContactEmail, getContact, isSuppressed, setPrimaryEmail, type ContactEmail } from "@/lib/api/contacts";
import { idempotencyKey } from "@/lib/api/client";
import { readEtag } from "@/lib/api/mutate";
import { ApiError } from "@/lib/api/problem";

export function ContactDetailScreen({ contactId }: { contactId: string }) {
  const t = useTranslations("contacts");
  const tc = useTranslations("common");
  const principals = useHumanPrincipals();
  const queryClient = useQueryClient();
  const etag = useRef<string | undefined>(undefined);
  const [editOpen, setEditOpen] = useState(false);
  const [composeRecipient, setComposeRecipient] =
    useState<ContactComposeRecipient | null>(null);
  const [address, setAddress] = useState("");
  const [label, setLabel] = useState("");
  const [primary, setPrimary] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const createIdem = useRef(idempotencyKey());

  const detail = useQuery({
    queryKey: ["contacts", "detail", contactId],
    queryFn: async () => {
      const result = await getContact(contactId);
      etag.current = readEtag(result) ?? String(result.data.version);
      return result.data;
    },
    retry: false,
  });
  const contact = detail.data;

  async function addAddress(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!contact || !address.trim()) return;
    setPending(true);
    setError(null);
    setNotice(null);
    try {
      const result = await addContactEmail(contact.id, { address: address.trim(), label: label.trim() || undefined, is_primary: primary }, { idempotencyKey: createIdem.current });
      createIdem.current = idempotencyKey();
      setNotice(result.data.linked_contact_ids?.length ? t("emails.linkedElsewhere", { count: result.data.linked_contact_ids.length }) : t("emails.addedUnverified"));
      setAddress("");
      setLabel("");
      setPrimary(false);
      await queryClient.invalidateQueries({ queryKey: ["contacts", "detail", contactId] });
      await detail.refetch();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.code === "EMAIL_ALREADY_LINKED" ? t("emails.conflict") : apiError?.problem.detail ?? t("emails.error"));
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) createIdem.current = idempotencyKey();
    } finally {
      setPending(false);
    }
  }

  async function makePrimary(email: ContactEmail) {
    if (!contact) return;
    setPending(true);
    setError(null);
    try {
      await setPrimaryEmail(contact.id, { email_address_id: email.email_address_id }, { etag: etag.current ?? contact.version, idempotencyKey: idempotencyKey() });
      await detail.refetch();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.status === 412 || apiError?.status === 428 ? t("form.conflictReload") : apiError?.problem.detail ?? t("emails.primaryError"));
    } finally {
      setPending(false);
    }
  }

  if (detail.isPending) return <div className="mx-auto flex w-full max-w-6xl flex-col gap-4 px-6 py-6" aria-busy="true"><Skeleton className="h-8 w-64" /><Skeleton className="h-28 w-full rounded-2xl" /><Skeleton className="h-72 w-full rounded-2xl" /></div>;
  if (detail.isError) return <div className="mx-auto max-w-6xl px-6 py-6"><ErrorState error={detail.error} onRetry={() => void detail.refetch()} /></div>;
  if (!contact) return null;
  const emails = contact.emails ?? [];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={contact.name} description={contact.company_name ?? t("detail.noCompany")} actions={<Button variant="outline" size="sm" onClick={() => setEditOpen(true)}>{t("detail.edit")}</Button>} />
      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(18rem,1fr)]">
        <Card>
          <CardHeader><CardTitle>{contact.name}</CardTitle></CardHeader>
          <CardContent>
            <dl className="grid gap-x-4 gap-y-3 text-sm sm:grid-cols-2">
              <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{t("detail.statusLabel")}</dt><dd><StatusBadge status={contact.status} /></dd></div>
              <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{t("detail.companyLabel")}</dt><dd>{contact.company_id ? <Link className="underline-offset-4 hover:underline" href={`/app/companies/${encodeURIComponent(contact.company_id)}`}>{contact.company_name ?? t("list.linkedCompany")}</Link> : t("detail.noCompany")}</dd></div>
              <Field label={t("detail.jobTitleLabel")} value={contact.job_title} />
              <Field label={t("detail.departmentLabel")} value={contact.department} />
              <Field label={t("detail.ownerLabel")} value={contact.owner_display_name?.trim() || principalName(principals.data?.find((p) => p.id === contact.owner_principal_id), contact.owner_principal_id) || (contact.owner_principal_id ? tc("nameUnavailable") : t("detail.unassigned"))} />
            </dl>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle>{t("detail.emailsTitle")}</CardTitle><p className="text-sm text-muted-foreground">{t("detail.emailsDescription")}</p></CardHeader>
          <CardContent className="flex flex-col gap-3">
            {emails.length === 0 ? <p className="text-sm text-muted-foreground">{t("emails.emptyDescription")}</p> : emails.map((email) => {
              const suppressed = isSuppressed(email);
              return <div key={email.email_address_id} className="rounded-2xl border border-border px-3 py-3">
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <div><p className="font-medium">{email.address}</p><p className="text-xs text-muted-foreground">{email.label ?? "—"}</p></div>
                  <div className="flex items-center gap-2">
                    {email.is_primary ? <span className="rounded-full bg-accent px-2.5 py-1 text-xs">{t("emails.primaryBadge")}</span> : <Button variant="outline" size="xs" disabled={pending} onClick={() => void makePrimary(email)}>{t("emails.setPrimary")}</Button>}
                    <Button
                      variant="outline"
                      size="xs"
                      onClick={() => setComposeRecipient({
                        contactName: contact.name,
                        emailAddressId: email.email_address_id,
                        address: email.address,
                      })}
                    >
                      {t("compose.action")}
                    </Button>
                  </div>
                </div>
                <dl className="mt-3 grid gap-2 text-xs sm:grid-cols-3">
                  <div><dt className="text-muted-foreground">{t("emails.verificationHeader")}</dt><dd className="mt-1"><StatusBadge status={email.verification_status} /></dd></div>
                  <div><dt className="text-muted-foreground">{t("emails.eligibilityHeader")}</dt><dd className="mt-1">{email.marketing_eligibility?.eligible ? t("emails.eligible") : t("emails.notEligible")}</dd>{email.marketing_eligibility?.reason_codes.map((code) => <p key={code} className="mt-1 text-muted-foreground">{t(`emails.reason.${code}` as never)}</p>)}</div>
                  <div><dt className="text-muted-foreground">{t("emails.suppressionHeader")}</dt><dd className="mt-1">{suppressed ? t("emails.suppressed") : "—"}</dd>{email.suppressions?.map((entry, index) => <p key={entry.id ?? index} className="mt-1 text-muted-foreground">{t("emails.suppressionReason", { reason: entry.reason ?? "—" })}</p>)}</div>
                </dl>
              </div>;
            })}
            <form className="grid gap-2 rounded-2xl border border-border p-3" onSubmit={(event) => void addAddress(event)}>
              <h3 className="text-sm font-medium">{t("emails.addTitle")}</h3>
              <div className="grid gap-1"><Label htmlFor="contact-email-address">{t("emails.address")}</Label><Input id="contact-email-address" type="email" required value={address} onChange={(event) => setAddress(event.target.value)} placeholder={t("emails.addressPlaceholder")} /></div>
              <div className="grid gap-1"><Label htmlFor="contact-email-label">{t("emails.label")}</Label><Input id="contact-email-label" value={label} onChange={(event) => setLabel(event.target.value)} placeholder={t("emails.labelPlaceholder")} /></div>
              <label className="flex items-center gap-2 text-sm"><Checkbox checked={primary} onCheckedChange={(checked) => setPrimary(checked === true)} />{t("emails.setPrimary")}</label>
              {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
              {notice ? <p role="status" className="text-sm text-success-on-subtle">{notice}</p> : null}
              <div className="flex justify-end"><Button type="submit" size="sm" disabled={pending || !address.trim()}>{pending ? t("emails.submitting") : t("emails.submit")}</Button></div>
            </form>
          </CardContent>
        </Card>
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Card><CardHeader><CardTitle>{t("detail.notesTitle")}</CardTitle></CardHeader><CardContent><NotesPanel ns="contacts" contactId={contactId} /></CardContent></Card>
        <Card><CardHeader><CardTitle>{t("detail.tasksTitle")}</CardTitle></CardHeader><CardContent><TasksPanel ns="contacts" contactId={contactId} /></CardContent></Card>
      </div>
      <ContactFormDialog open={editOpen} onOpenChange={setEditOpen} contact={contact} etag={contact.version} onSaved={() => { void detail.refetch(); void queryClient.invalidateQueries({ queryKey: ["contacts"] }); }} />
      {composeRecipient ? (
        <ContactComposeDialog
          open
          onOpenChange={(open) => {
            if (!open) setComposeRecipient(null);
          }}
          recipient={composeRecipient}
        />
      ) : null}
    </div>
  );
}

function Field({ label, value }: { label: string; value: string | null | undefined }) {
  return <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{label}</dt><dd>{value || "—"}</dd></div>;
}
