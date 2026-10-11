"use client";

import { useTranslations } from "next-intl";

import { PageHeader } from "@/components/patterns/page-header";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ContactProfile } from "@/features/contacts/contact-profile";
import { NotesPanel } from "@/features/companies/notes-panel";
import { TasksPanel } from "@/features/companies/tasks-panel";

export function ContactDetailScreen({ contactId }: { contactId: string }) {
  const t = useTranslations("contacts");

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <ContactProfile
        contactId={contactId}
        renderHeader={(contact, openEdit) => <PageHeader title={contact.name} description={contact.company_name ?? t("detail.noCompany")} actions={<Button variant="outline" size="sm" onClick={openEdit}>{t("detail.edit")}</Button>} />}
      />
      <div className="grid gap-4 lg:grid-cols-2">
        <Card><CardHeader><CardTitle>{t("detail.notesTitle")}</CardTitle></CardHeader><CardContent><NotesPanel ns="contacts" contactId={contactId} /></CardContent></Card>
        <Card><CardHeader><CardTitle>{t("detail.tasksTitle")}</CardTitle></CardHeader><CardContent><TasksPanel ns="contacts" contactId={contactId} /></CardContent></Card>
      </div>
    </div>
  );
}
