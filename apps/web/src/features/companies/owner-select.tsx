"use client";

/**
 * 담당자(owner_principal_id) 선택 — GET /principals 목록에서 활성 human만 보인다.
 * 서버가 최종 권한을 검사하므로 이 목록은 표시 힌트다 (B.3.3 재배정 권한).
 */
import { useQuery } from "@tanstack/react-query";
import { useTranslations } from "next-intl";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { getMe } from "@/lib/api/auth";
import { listPrincipals, type Principal } from "@/lib/api/companies";

const UNASSIGNED = "__unassigned__";

/** /me 캐시 — 본인 principal 표시와 role 기반 힌트에 쓴다. */
export function useMe() {
  return useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    staleTime: 60_000,
  });
}

/** human principal 목록을 캐시해서 owner 필터/선택이 공유한다. */
export function useHumanPrincipals() {
  return useQuery({
    queryKey: ["principals", "human"],
    queryFn: async () => {
      const res = await listPrincipals();
      return (res.data ?? []).filter(
        (p) => p.kind === "human" && (p.status === undefined || p.status === "active")
      );
    },
    staleTime: 60_000,
  });
}

export function principalName(p: Principal | undefined, id: string | null | undefined): string | null {
  if (!id) return null;
  if (!p) return null;
  return p.display_name ?? null;
}

interface OwnerSelectProps {
  value: string | null | undefined;
  onChange: (value: string | null) => void;
  unassignedLabel: string;
  /** 본인 항목에 붙일 표시 — 예: "본인". */
  meLabel?: string;
  id?: string;
  disabled?: boolean;
}

export function OwnerSelect({
  value,
  onChange,
  unassignedLabel,
  meLabel,
  id,
  disabled,
}: OwnerSelectProps) {
  const t = useTranslations("common");
  const principals = useHumanPrincipals();
  const me = useMe();
  const meId = me.data?.principal_id;
  const items = principals.data ?? [];

  return (
    <Select
      value={value ?? UNASSIGNED}
      onValueChange={(v) => onChange(v === UNASSIGNED ? null : v)}
      disabled={disabled}
    >
      <SelectTrigger id={id} aria-label={unassignedLabel} className="w-full">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value={UNASSIGNED}>{unassignedLabel}</SelectItem>
        {items.map((p) => (
          <SelectItem key={p.id} value={p.id}>
            {p.id === meId && meLabel
              ? `${p.display_name ?? p.id} (${meLabel})`
              : (p.display_name ?? p.id)}
          </SelectItem>
        ))}
        {principals.isError ? (
          <SelectItem value="__error__" disabled>
            {t("error.title")}
          </SelectItem>
        ) : null}
      </SelectContent>
    </Select>
  );
}
