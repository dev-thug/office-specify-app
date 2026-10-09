"use client";

import { useTranslations } from "next-intl";
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronDownIcon, ChevronRightIcon, SearchIcon } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  CONTACTS_PATH,
  audienceEmailAddress,
  audienceEmailId,
  contactEmails,
  getContact,
  type AudienceContact,
  type AudienceEmail,
} from "@/lib/api/campaigns";
import { useCursorList } from "@/lib/api/use-cursor-list";

/** 선택된 발송 대상 — key 는 email_address_id (주소당 1건). */
export type AudienceSelection = ReadonlyMap<
  string,
  {
    email_address_id: string;
    address?: string;
    contact_id?: string;
    contact_name?: string;
  }
>;

function contactDisplayName(c: AudienceContact): string {
  return c.name ?? c.display_name ?? c.id;
}

function companyName(c: AudienceContact): string | undefined {
  return c.company_name ?? c.company?.name ?? undefined;
}

/** 이메일 형식 유효와 발송 허가는 다른 배지다 (§7.3 UI04 규칙). */
function EmailBadges({ email }: { email: AudienceEmail }) {
  const t = useTranslations("campaigns.audience");
  const tReason = useTranslations("campaigns.reasonCodes");
  const verification = email.verification_status;
  const eligible = email.marketing_eligibility?.eligible;
  const reasons = email.marketing_eligibility?.reason_codes ?? [];

  return (
    <span className="flex flex-wrap items-center gap-1">
      {email.is_primary ? (
        <Badge variant="secondary">{t("primary")}</Badge>
      ) : null}
      {verification === "valid" || verification === "verified" ? (
        <Badge variant="success-subtle">{t("formatValid")}</Badge>
      ) : verification === "invalid" ? (
        <Badge variant="rose-subtle">{t("formatInvalid")}</Badge>
      ) : verification ? (
        <Badge variant="secondary">{verification}</Badge>
      ) : null}
      {eligible === true ? (
        <Badge variant="info-subtle">{t("sendPermitted")}</Badge>
      ) : eligible === false ? (
        <Badge
          variant="amber-subtle"
          title={reasons
            .map((r) => (tReason.has(r) ? tReason(r) : r))
            .join(", ")}
        >
          {t("sendNotPermitted")}
        </Badge>
      ) : null}
    </span>
  );
}

function ContactEmails({
  contact,
  selected,
  onToggle,
}: {
  contact: AudienceContact;
  selected: AudienceSelection;
  onToggle: (
    entry: {
      email_address_id: string;
      address?: string;
      contact_id?: string;
      contact_name?: string;
    },
    checked: boolean
  ) => void;
}) {
  const t = useTranslations("campaigns.audience");

  // 목록 행에 주소가 내장되어 있으면 그대로 쓰고, 없으면 단건 조회로 가져온다.
  const embedded = contactEmails(contact);
  const detail = useQuery({
    queryKey: ["campaigns", "audience-contact", contact.id],
    enabled: embedded.length === 0,
    queryFn: async () => (await getContact(contact.id)).data,
    retry: false,
  });
  const emails = embedded.length > 0 ? embedded : contactEmails(detail.data);

  if (embedded.length === 0 && detail.isLoading) {
    return (
      <div className="flex flex-col gap-1.5 py-1">
        <Skeleton className="h-7 w-full rounded-lg" />
        <Skeleton className="h-7 w-2/3 rounded-lg" />
      </div>
    );
  }
  if (embedded.length === 0 && detail.isError) {
    return (
      <ErrorState error={detail.error} onRetry={() => detail.refetch()} />
    );
  }
  if (emails.length === 0) {
    return (
      <p className="py-1 text-sm text-muted-foreground">{t("noEmails")}</p>
    );
  }

  return (
    <ul className="flex flex-col gap-1.5 py-1">
      {emails.map((email) => {
        const id = audienceEmailId(email);
        if (!id) return null;
        const address = audienceEmailAddress(email);
        const checked = selected.has(id);
        return (
          <li key={id}>
            <label className="flex cursor-pointer flex-wrap items-center gap-2 rounded-lg px-2 py-1.5 text-sm hover:bg-muted/60">
              <Checkbox
                checked={checked}
                onCheckedChange={(v) =>
                  onToggle(
                    {
                      email_address_id: id,
                      address,
                      contact_id: contact.id,
                      contact_name: contactDisplayName(contact),
                    },
                    v === true
                  )
                }
                aria-label={address ?? id}
              />
              <span className="font-mono text-ui">{address ?? id}</span>
              <EmailBadges email={email} />
            </label>
          </li>
        );
      })}
    </ul>
  );
}

export function AudiencePicker({
  selected,
  onChange,
}: {
  selected: AudienceSelection;
  onChange: (next: Map<string, { email_address_id: string; address?: string; contact_id?: string; contact_name?: string }>) => void;
}) {
  const t = useTranslations("campaigns.audience");
  const tCommon = useTranslations("common");
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const list = useCursorList<AudienceContact>({
    queryKey: ["campaigns", "audience-contacts"],
    path: CONTACTS_PATH,
    limit: 100,
  });

  const rows = useMemo(
    () => list.data?.pages.flatMap((p) => p.data) ?? [],
    [list.data]
  );

  // 담당자 검색 파라미터는 문서화되어 있지 않아 로드된 범위에서 필터한다.
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((c) =>
      [contactDisplayName(c), companyName(c)]
        .filter(Boolean)
        .some((v) => v!.toLowerCase().includes(q))
    );
  }, [rows, query]);

  const lastPage = list.data?.pages[list.data.pages.length - 1];

  function toggleEntry(
    entry: {
      email_address_id: string;
      address?: string;
      contact_id?: string;
      contact_name?: string;
    },
    checked: boolean
  ) {
    const next = new Map(selected);
    if (checked) next.set(entry.email_address_id, entry);
    else next.delete(entry.email_address_id);
    onChange(next);
  }

  function toggleExpanded(id: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  return (
    <section
      aria-label={t("title")}
      className="flex flex-col gap-3 rounded-2xl border border-border bg-card px-4 py-4"
    >
      <div>
        <h3 className="text-sm font-medium">{t("title")}</h3>
        <p className="mt-0.5 text-2xs text-muted-foreground">
          {t("description")}
        </p>
      </div>

      <div className="relative max-w-sm">
        <SearchIcon
          aria-hidden
          className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
        />
        <Input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("searchPlaceholder")}
          aria-label={t("searchPlaceholder")}
          className="pl-9"
        />
      </div>

      <div className="flex items-center justify-between gap-2 text-sm">
        <span className="font-medium">
          {t("selectedCount", { count: selected.size })}
        </span>
        {selected.size > 0 ? (
          <Button
            type="button"
            variant="ghost"
            size="xs"
            onClick={() => onChange(new Map())}
          >
            {t("clearSelection")}
          </Button>
        ) : null}
      </div>
      <p className="text-2xs text-muted-foreground">{t("dupNote")}</p>

      {list.isLoading ? (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded-lg" />
          ))}
        </div>
      ) : list.isError ? (
        <ErrorState error={list.error} onRetry={() => list.refetch()} />
      ) : filtered.length === 0 ? (
        <EmptyState
          compact
          title={t("emptyTitle")}
          description={t("emptyDescription")}
        />
      ) : (
        <ul className="flex flex-col gap-1.5">
          {filtered.map((c) => {
            const open = expanded.has(c.id);
            return (
              <li
                key={c.id}
                className="rounded-lg border border-border px-3 py-2"
              >
                <button
                  type="button"
                  className="flex w-full items-center gap-2 text-left text-sm"
                  aria-expanded={open}
                  onClick={() => toggleExpanded(c.id)}
                >
                  {open ? (
                    <ChevronDownIcon className="size-4 shrink-0" aria-hidden />
                  ) : (
                    <ChevronRightIcon className="size-4 shrink-0" aria-hidden />
                  )}
                  <span className="min-w-0 flex-1">
                    <span className="font-medium">{contactDisplayName(c)}</span>
                    {companyName(c) ? (
                      <span className="ml-2 text-muted-foreground">
                        {companyName(c)}
                      </span>
                    ) : null}
                  </span>
                  <span className="shrink-0 text-2xs text-muted-foreground">
                    {t("loadEmails")}
                  </span>
                </button>
                {open ? (
                  <ContactEmails
                    contact={c}
                    selected={selected}
                    onToggle={toggleEntry}
                  />
                ) : null}
              </li>
            );
          })}
        </ul>
      )}

      {lastPage?.page?.has_more ? (
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={list.isFetchingNextPage}
            onClick={() => list.fetchNextPage()}
          >
            {list.isFetchingNextPage
              ? tCommon("table.loading")
              : tCommon("table.loadMore")}
          </Button>
        </div>
      ) : null}
    </section>
  );
}
