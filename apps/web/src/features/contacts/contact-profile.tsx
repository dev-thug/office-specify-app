"use client";

/**
 * 담당자 상세 본문 — 기본 정보, 이메일 주소(추가·주 주소·연결 해제), 수정/메일 작성 다이얼로그.
 * 상세 페이지와 회사 화면의 사이드 패널이 함께 쓴다. 헤더는 호출자가 renderHeader 로 그린다.
 */
import Link from "next/link";
import { useRef, useState, type FormEvent, type ReactNode } from "react";
import { useTranslations } from "next-intl";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { ErrorState } from "@/components/patterns/error-state";
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
import { principalName, useHumanPrincipals } from "@/features/companies/owner-select";
import {
  addContactEmail,
  getContact,
  isSuppressed,
  removeContactEmail,
  setPrimaryEmail,
  type Contact,
  type ContactEmail,
} from "@/lib/api/contacts";
import { idempotencyKey } from "@/lib/api/client";
import { readEtag } from "@/lib/api/mutate";
import { ApiError } from "@/lib/api/problem";
import { cn } from "@/lib/utils";

export function ContactProfile({
  contactId,
  compact = false,
  renderHeader,
  renderLoading,
  className,
}: {
  contactId: string;
  /** 사이드 패널처럼 좁은 영역 — 카드를 한 줄로 쌓는다. */
  compact?: boolean;
  renderHeader: (contact: Contact, openEdit: () => void) => ReactNode;
  /** 불러오는 중·오류일 때 헤더 자리에 그릴 내용. */
  renderLoading?: () => ReactNode;
  /** 헤더 아래 본문 영역 클래스. */
  className?: string;
}) {
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

  // 담당자 카드·목록도 같은 값을 보여 주므로 함께 갱신한다.
  async function refresh() {
    await queryClient.invalidateQueries({ queryKey: ["contacts"] });
  }

  function versionError(cause: unknown, fallback: string) {
    const apiError = cause instanceof ApiError ? cause : null;
    setError(apiError?.status === 412 || apiError?.status === 428 ? t("form.conflictReload") : apiError?.problem.detail ?? fallback);
  }

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
      await refresh();
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
    setNotice(null);
    try {
      const result = await setPrimaryEmail(contact.id, { email_address_id: email.email_address_id }, { etag: etag.current ?? contact.version, idempotencyKey: idempotencyKey() });
      etag.current = readEtag(result) ?? String(result.data.version);
      await refresh();
    } catch (cause) {
      versionError(cause, t("emails.primaryError"));
    } finally {
      setPending(false);
    }
  }

  async function unlink(email: ContactEmail) {
    if (!contact) return;
    if (!window.confirm(t("emails.removeConfirm", { address: email.address }))) return;
    setPending(true);
    setError(null);
    setNotice(null);
    try {
      const result = await removeContactEmail(contact.id, email.email_address_id, { etag: etag.current ?? contact.version, idempotencyKey: idempotencyKey() });
      etag.current = readEtag(result) ?? String(result.data.version);
      setNotice(t("emails.removed"));
      await refresh();
    } catch (cause) {
      versionError(cause, t("emails.removeError"));
    } finally {
      setPending(false);
    }
  }

  if (detail.isPending) return <>{renderLoading?.()}<div className={cn("flex flex-col gap-4", className)} aria-busy="true"><Skeleton className="h-8 w-64" /><Skeleton className="h-28 w-full rounded-2xl" /><Skeleton className="h-72 w-full rounded-2xl" /></div></>;
  if (detail.isError) return <>{renderLoading?.()}<div className={className}><ErrorState error={detail.error} onRetry={() => void detail.refetch()} /></div></>;
  if (!contact) return null;
  const emails = contact.emails ?? [];
  const ownerName = contact.owner_display_name?.trim() || principalName(principals.data?.find((p) => p.id === contact.owner_principal_id), contact.owner_principal_id) || (contact.owner_principal_id ? tc("nameUnavailable") : t("detail.unassigned"));

  return (
    <>
      {renderHeader(contact, () => setEditOpen(true))}
      <div className={cn("grid gap-4", !compact && "lg:grid-cols-[minmax(0,1fr)_minmax(18rem,1fr)]", className)}>
        <Card>
          <CardHeader><CardTitle>{compact ? t("detail.infoTitle") : contact.name}</CardTitle></CardHeader>
          <CardContent>
            <dl className="grid gap-x-4 gap-y-3 text-sm sm:grid-cols-2">
              <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{t("detail.statusLabel")}</dt><dd><StatusBadge status={contact.status} /></dd></div>
              <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{t("detail.companyLabel")}</dt><dd>{contact.company_id ? <Link className="underline-offset-4 hover:underline" href={`/app/companies/${encodeURIComponent(contact.company_id)}`}>{contact.company_name ?? t("list.linkedCompany")}</Link> : t("detail.noCompany")}</dd></div>
              <Field label={t("detail.jobTitleLabel")} value={contact.job_title} />
              <Field label={t("detail.departmentLabel")} value={contact.department} />
              <Field label={t("form.phone")} value={contact.phone ?? null} />
              <Field label={t("detail.ownerLabel")} value={ownerName} />
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
                  <div className="min-w-0"><p className="break-all font-medium">{email.address}</p><p className="text-xs text-muted-foreground">{email.label ?? "—"}</p></div>
                  <div className="flex flex-wrap items-center gap-2">
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
                    <Button variant="ghost" size="xs" className="text-muted-foreground" disabled={pending} onClick={() => void unlink(email)}>{t("emails.remove")}</Button>
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
              <div className="grid gap-1"><Label htmlFor={`contact-email-address-${contact.id}`}>{t("emails.address")}</Label><Input id={`contact-email-address-${contact.id}`} type="email" required value={address} onChange={(event) => setAddress(event.target.value)} placeholder={t("emails.addressPlaceholder")} /></div>
              <div className="grid gap-1"><Label htmlFor={`contact-email-label-${contact.id}`}>{t("emails.label")}</Label><Input id={`contact-email-label-${contact.id}`} value={label} onChange={(event) => setLabel(event.target.value)} placeholder={t("emails.labelPlaceholder")} /></div>
              <label className="flex items-center gap-2 text-sm"><Checkbox checked={primary} onCheckedChange={(checked) => setPrimary(checked === true)} />{t("emails.setPrimary")}</label>
              {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
              {notice ? <p role="status" className="text-sm text-success-on-subtle">{notice}</p> : null}
              <div className="flex justify-end"><Button type="submit" size="sm" disabled={pending || !address.trim()}>{pending ? t("emails.submitting") : t("emails.submit")}</Button></div>
            </form>
          </CardContent>
        </Card>
      </div>
      <ContactFormDialog open={editOpen} onOpenChange={setEditOpen} contact={contact} etag={contact.version} onSaved={() => void refresh()} />
      {composeRecipient ? (
        <ContactComposeDialog
          open
          onOpenChange={(open) => {
            if (!open) setComposeRecipient(null);
          }}
          recipient={composeRecipient}
        />
      ) : null}
    </>
  );
}

function Field({ label, value }: { label: string; value: string | null | undefined }) {
  return <div className="grid gap-1"><dt className="text-xs text-muted-foreground">{label}</dt><dd>{value || "—"}</dd></div>;
}
