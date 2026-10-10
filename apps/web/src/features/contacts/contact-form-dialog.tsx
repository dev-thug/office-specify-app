"use client";

import { useRef, useState, type FormEvent } from "react";
import { useTranslations } from "next-intl";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { CompanyPicker } from "@/features/companies/company-picker";
import { OwnerSelect } from "@/features/companies/owner-select";
import { createContact, patchContact, type Contact, type CreateContactInput, type PatchContactInput } from "@/lib/api/contacts";
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
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <ContactFormBody
        key={`${contact?.id ?? "new"}:${defaultCompany?.id ?? ""}`}
        contact={contact}
        defaultCompany={defaultCompany}
        etag={etag}
        t={t}
        tc={tc}
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
  onClose,
  onSaved,
}: {
  contact?: Contact;
  defaultCompany?: Company;
  etag?: string | number;
  t: ReturnType<typeof useTranslations>;
  tc: ReturnType<typeof useTranslations>;
  onClose: () => void;
  onSaved?: (contact: Contact, mode: "created" | "updated") => void;
}) {
  const [name, setName] = useState(contact?.name ?? "");
  const [jobTitle, setJobTitle] = useState(contact?.job_title ?? "");
  const [department, setDepartment] = useState(contact?.department ?? "");
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
        };
        saved = (await createContact(input, { idempotencyKey: idem.current })).data;
      }
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
    <DialogContent>
      <DialogHeader>
        <DialogTitle>{editing ? t("editTitle") : t("createTitle")}</DialogTitle>
        <DialogDescription>{t("namePlaceholder")}</DialogDescription>
      </DialogHeader>
      <form className="grid gap-3" onSubmit={(event) => void submit(event)}>
        <div className="grid gap-1.5">
          <Label htmlFor="contact-name">{t("name")}</Label>
          <Input id="contact-name" autoFocus required value={name} onChange={(event) => setName(event.target.value)} maxLength={300} />
        </div>
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
