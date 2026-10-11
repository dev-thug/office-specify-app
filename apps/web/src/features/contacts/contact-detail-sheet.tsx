"use client";

/** 회사 화면 등에서 담당자 카드를 눌렀을 때 여는 상세·편집 패널. */
import Link from "next/link";
import { ExternalLink } from "lucide-react";
import { useState } from "react";
import { useTranslations } from "next-intl";

import { Button, buttonVariants } from "@/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { ContactProfile } from "@/features/contacts/contact-profile";
import { cn } from "@/lib/utils";

interface SheetContact {
  id: string;
  name: string;
}

export function ContactDetailSheet({
  contact,
  onOpenChange,
}: {
  /** null 이면 닫힌 상태 — 선택된 담당자가 있을 때만 조회한다. */
  contact: SheetContact | null;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("contacts");
  // 닫히는 애니메이션 동안 내용이 비지 않도록 마지막 담당자를 유지한다.
  const [shown, setShown] = useState<SheetContact | null>(contact);
  if (contact && contact.id !== shown?.id) setShown(contact);

  return (
    <Sheet open={contact !== null} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full overflow-y-auto sm:max-w-xl">
        {shown ? (
          <ContactProfile
            key={shown.id}
            contactId={shown.id}
            compact
            renderHeader={(loaded, openEdit) => (
              <SheetHeader>
                <SheetTitle>{loaded.name}</SheetTitle>
                <SheetDescription>{[loaded.job_title, loaded.department].filter(Boolean).join(" · ") || loaded.company_name || t("detail.noCompany")}</SheetDescription>
                <div className="mt-2 flex flex-wrap gap-2">
                  <Button size="sm" onClick={openEdit}>{t("detail.edit")}</Button>
                  <Link href={`/app/contacts/${encodeURIComponent(loaded.id)}`} className={cn(buttonVariants({ variant: "outline", size: "sm" }))}>
                    <ExternalLink aria-hidden />
                    {t("detail.openFullPage")}
                  </Link>
                </div>
              </SheetHeader>
            )}
            renderLoading={() => (
              <SheetHeader>
                <SheetTitle>{shown.name}</SheetTitle>
                <SheetDescription className="sr-only">{t("detail.infoTitle")}</SheetDescription>
              </SheetHeader>
            )}
            className="px-4 pb-6"
          />
        ) : null}
      </SheetContent>
    </Sheet>
  );
}
