"use client";

import { useRef, useState, type FormEvent } from "react";
import { useTranslations } from "next-intl";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { CompanyPicker } from "@/features/companies/company-picker";
import { OwnerSelect } from "@/features/companies/owner-select";
import { CONTACT_STATUSES, createContact, patchContact, type Contact, type CreateContactInput, type PatchContactInput } from "@/lib/api/contacts";
import type { Company } from "@/lib/api/companies";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

export function ContactFormDialog({
  open,
  onOpenChange,
  contact,
  defaultCompany,
  etag,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  contact?: Contact;
  defaultCompany?: Company;
  etag?: string | number;
  onSaved?: (contact: Contact, mode: "created" | "updated") => void;
}) {
  const t = useTranslations("contacts.form");
  const tc = useTranslations("common");
  const ts = useTranslations("status");
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <ContactFormBody
        key={`${contact?.id ?? "new"}:${contact?.version ?? ""}:${defaultCompany?.id ?? ""}`}
        contact={contact}
        defaultCompany={defaultCompany}
        etag={etag}
        t={t}
        tc={tc}
        ts={ts}
        onClose={() => onOpenChange(false)}
        onSaved={onSaved}
      />
    </Dialog>
  );
}

function ContactFormBody({
  contact,
  defaultCompany,
  etag,
  t,
  tc,
  ts,
  onClose,
  onSaved,
}: {
  contact?: Contact;
  defaultCompany?: Company;
  etag?: string | number;
  t: ReturnType<typeof useTranslations>;
  tc: ReturnType<typeof useTranslations>;
  ts: ReturnType<typeof useTranslations>;
  onClose: () => void;
  onSaved?: (contact: Contact, mode: "created" | "updated") => void;
}) {
  const [name, setName] = useState(contact?.name ?? "");
  const [email, setEmail] = useState("");
  const [phone, setPhone] = useState(contact?.phone ?? "");
  const [jobTitle, setJobTitle] = useState(contact?.job_title ?? "");
  const [department, setDepartment] = useState(contact?.department ?? "");
  const [status, setStatus] = useState<string>(contact?.status ?? "active");
  const [company, setCompany] = useState<Company | null>(
    contact?.company_id ? { id: contact.company_id, name: contact.company_name?.trim() || tc("nameUnavailable") } as Company : defaultCompany ?? null
  );
  const [ownerId, setOwnerId] = useState<string | null>(contact?.owner_principal_id ?? null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idem = useRef(idempotencyKey());
  const editing = Boolean(contact);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!name.trim()) return;
    setPending(true);
    setError(null);
    try {
      let saved: Contact;
      if (contact) {
        const patch: PatchContactInput = {
          name: name.trim(),
          company_id: company?.id ?? null,
          job_title: jobTitle.trim() || null,
          department: department.trim() || null,
          phone: phone.trim() || null,
          ...(status !== contact.status ? { status } : {}),
        };
        saved = (await patchContact(contact.id, patch, {
          etag: etag ?? contact.version,
          idempotencyKey: idem.current,
        })).data;
      } else {
        const input: CreateContactInput = {
          name: name.trim(),
          company_id: company?.id,
          job_title: jobTitle.trim() || undefined,
          department: department.trim() || undefined,
          owner_principal_id: ownerId ?? undefined,
          email: email.trim() || undefined,
          phone: phone.trim() || undefined,
        };
        saved = (await createContact(input, { idempotencyKey: idem.current })).data;
      }
      // 다음 저장은 별개 작업이다 — 같은 키를 다시 보내면 이전 요청으로 취급된다.
      idem.current = idempotencyKey();
      onSaved?.(saved, editing ? "updated" : "created");
      onClose();
    } catch (cause) {
      const apiError = cause instanceof ApiError ? cause : null;
      setError(apiError?.status === 412 ? tc("conflict.description") : apiError?.problem.detail ?? t("error"));
      if (apiError && apiError.problem.retryable !== true && apiError.status < 500) idem.current = idempotencyKey();
    } finally {
      setPending(false);
    }
  }

  return (
    <DialogContent className="max-h-[90dvh] overflow-y-auto">
      <DialogHeader>
        <DialogTitle>{editing ? t("editTitle") : t("createTitle")}</DialogTitle>
        <DialogDescription>{t("namePlaceholder")}</DialogDescription>
      </DialogHeader>
      <form className="grid gap-3" onSubmit={(event) => void submit(event)}>
        <div className="grid gap-1.5">
          <Label htmlFor="contact-name">{t("name")}</Label>
          <Input id="contact-name" autoFocus required value={name} onChange={(event) => setName(event.target.value)} maxLength={300} />
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          {!editing ? <div className="grid gap-1.5">
            <Label htmlFor="contact-email">{t("email")}</Label>
            <Input id="contact-email" type="email" autoComplete="email" value={email} onChange={event => setEmail(event.target.value)} placeholder="name@company.com" maxLength={320} />
          </div> : null}
          <div className="grid gap-1.5">
            <Label htmlFor="contact-phone">{t("phone")}</Label>
            <Input id="contact-phone" type="tel" autoComplete="tel" value={phone} onChange={event => setPhone(event.target.value)} placeholder={t("phonePlaceholder")} maxLength={80} />
          </div>
        </div>
        {!editing ? <p className="text-xs text-muted-foreground">{t("contactInfoHint")}</p> : null}
        <div className="grid gap-1.5">
          <Label>{t("company")}</Label>
          <CompanyPicker value={company} onChange={setCompany} placeholder={t("companyPlaceholder")} noneLabel={t("companyNone")} emptyLabel={t("companySearchEmpty")} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="contact-title">{t("jobTitle")}</Label>
          <Input id="contact-title" value={jobTitle} onChange={(event) => setJobTitle(event.target.value)} maxLength={300} />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="contact-department">{t("department")}</Label>
          <Input id="contact-department" value={department} onChange={(event) => setDepartment(event.target.value)} maxLength={300} />
        </div>
        {editing ? <div className="grid gap-1.5">
          <Label htmlFor="contact-status">{t("status")}</Label>
          <Select value={status} onValueChange={setStatus}>
            <SelectTrigger id="contact-status" className="w-full"><SelectValue /></SelectTrigger>
            <SelectContent>
              {CONTACT_STATUSES.map((value) => <SelectItem key={value} value={value}>{ts(value)}</SelectItem>)}
            </SelectContent>
          </Select>
        </div> : null}
        <div className="grid gap-1.5">
          <Label htmlFor="contact-owner">{t("owner")}</Label>
          <OwnerSelect id="contact-owner" value={ownerId} onChange={setOwnerId} unassignedLabel={t("ownerUnassigned")} disabled={editing} />
          {contact ? <p className="text-2xs text-muted-foreground">{t("ownerFixed")}</p> : null}
        </div>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose}>{tc("cancel")}</Button>
          <Button type="submit" disabled={pending || !name.trim()}>{pending ? t("saving") : editing ? t("save") : t("submit")}</Button>
        </DialogFooter>
      </form>
    </DialogContent>
  );
}
