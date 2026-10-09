"use client";

/**
 * 메일함 grant 편집 시트 — §5.4 권한 표 + B.3.2 낙관적 잠금.
 * - compose→read, send→compose, approve→send, assign→read 의존을 UI에서 강제한다.
 * - 에이전트/비관리 역할에는 부여 불가 스위치를 비활성화한다.
 * - 기존 행 PUT If-Match:"<version>", 신규 행 PUT If-None-Match:* — 412 는
 *   갱신 후 충돌 안내로 표면에 올린다.
 */
import { useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  putMailAccountGrant,
  useMailAccountGrants,
  usePrincipals,
  type GrantBooleans,
  type MailAccount,
  type MailAccountGrant,
  type Principal,
} from "@/lib/api/settings";
import { apiErrorText } from "@/features/settings/api-error";
import {
  applyGrantChange,
  grantBlockReason,
  type GrantKey,
} from "@/features/settings/grant-rules";
import { useIsAdmin } from "@/features/settings/use-me";

const GRANT_KEYS: GrantKey[] = [
  "can_read",
  "can_compose",
  "can_send",
  "can_approve",
  "can_assign",
];

const DEFAULT_GRANT: GrantBooleans = {
  can_read: true,
  can_compose: false,
  can_send: false,
  can_approve: false,
  can_assign: false,
};

function booleansOf(g: MailAccountGrant): GrantBooleans {
  return {
    can_read: g.can_read,
    can_compose: g.can_compose,
    can_send: g.can_send,
    can_approve: g.can_approve,
    can_assign: g.can_assign,
  };
}

export function GrantsSheet({
  account,
  open,
  onOpenChange,
}: {
  account: MailAccount | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("settings.mailAccounts.grants");
  const tSettings = useTranslations("settings");
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();

  const grants = useMailAccountGrants(account?.id);
  const rows = grants.data?.pages.flatMap((p) => p.data) ?? [];

  const principals = usePrincipals();
  const principalMap = new Map(
    principals.data?.pages.flatMap((p) => p.data).map((p) => [p.id, p]) ?? []
  );

  // 행별 편집 상태 — 저장 전까지 서버 값을 덮어쓰지 않는다.
  const [edits, setEdits] = useState<Record<string, GrantBooleans>>({});

  const save = useMutation({
    mutationFn: ({ grant, create }: { grant: MailAccountGrant; create: boolean }) =>
      putMailAccountGrant(
        account!.id,
        grant.principal_id,
        edits[grant.principal_id] ?? booleansOf(grant),
        {
          etag: grant.version,
          idempotencyKey: idempotencyKey(),
          create,
        }
      ),
    onSuccess: (_res, { grant }) => {
      toast.success(t("saveSuccess"));
      setEdits((prev) => {
        const next = { ...prev };
        delete next[grant.principal_id];
        return next;
      });
      invalidate();
    },
    onError: (err) => {
      // 412 VERSION_MISMATCH 포함 — 목록을 갱신해 최신 버전을 다시 받는다.
      toast.error(apiErrorText(tSettings, err));
      invalidate();
    },
  });

  function invalidate() {
    queryClient.invalidateQueries({
      queryKey: ["settings", "mail-accounts", account?.id, "grants"],
    });
  }

  const grantedIds = new Set(rows.map((r) => r.principal_id));
  const candidates = [...principalMap.values()].filter(
    (p) => p.status === "active" && !grantedIds.has(p.id) && p.kind !== "system"
  );

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full overflow-y-auto sm:max-w-2xl">
        <SheetHeader>
          <SheetTitle>{t("title")}</SheetTitle>
          <SheetDescription>
            {account ? `${account.name} — ${account.primary_address}` : ""}
          </SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-4 px-4 pb-6">
          <p className="text-2xs text-muted-foreground">{t("rulesNotice")}</p>
          {!isAdmin ? (
            <p className="text-2xs text-muted-foreground">{t("readOnly")}</p>
          ) : null}

          {grants.isPending ? (
            <div className="flex flex-col gap-2" aria-busy="true">
              {Array.from({ length: 3 }).map((_, i) => (
                <Skeleton key={i} className="h-24 w-full rounded-2xl" />
              ))}
            </div>
          ) : grants.isError ? (
            <ErrorState error={grants.error} onRetry={() => grants.refetch()} />
          ) : rows.length === 0 ? (
            <EmptyState title={t("emptyTitle")} compact />
          ) : (
            <ul className="flex flex-col gap-3">
              {rows.map((row) => (
                <GrantRow
                  key={row.principal_id}
                  grant={row}
                  principal={principalMap.get(row.principal_id)}
                  value={edits[row.principal_id] ?? booleansOf(row)}
                  readOnly={!isAdmin}
                  pending={save.isPending}
                  onChange={(next) =>
                    setEdits((prev) => ({ ...prev, [row.principal_id]: next }))
                  }
                  onSave={() => save.mutate({ grant: row, create: false })}
                  dirty={
                    edits[row.principal_id] !== undefined &&
                    JSON.stringify(edits[row.principal_id]) !==
                      JSON.stringify(booleansOf(row))
                  }
                />
              ))}
            </ul>
          )}

          {isAdmin && account ? (
            <NewGrantForm
              accountId={account.id}
              candidates={candidates}
              onSaved={invalidate}
            />
          ) : null}
        </div>
      </SheetContent>
    </Sheet>
  );
}

function GrantRow({
  grant,
  principal,
  value,
  readOnly,
  pending,
  dirty,
  onChange,
  onSave,
}: {
  grant: MailAccountGrant;
  principal: Principal | undefined;
  value: GrantBooleans;
  readOnly: boolean;
  pending: boolean;
  dirty: boolean;
  onChange: (next: GrantBooleans) => void;
  onSave: () => void;
}) {
  const t = useTranslations("settings.mailAccounts.grants");
  return (
    <li className="flex flex-col gap-2 rounded-2xl border border-border p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm font-medium">
          {principal?.name ?? grant.principal_name ?? grant.principal_id.slice(0, 8)}
        </span>
        {!readOnly ? (
          <Button
            type="button"
            variant="outline"
            size="xs"
            disabled={!dirty || pending}
            onClick={onSave}
          >
            {t("save")}
          </Button>
        ) : null}
      </div>
      <GrantToggles
        value={value}
        principal={principal}
        readOnly={readOnly}
        idPrefix={`grant-${grant.principal_id}`}
        onChange={onChange}
      />
    </li>
  );
}

function GrantToggles({
  value,
  principal,
  readOnly,
  idPrefix,
  onChange,
}: {
  value: GrantBooleans;
  principal: Principal | undefined;
  readOnly: boolean;
  idPrefix: string;
  onChange: (next: GrantBooleans) => void;
}) {
  const t = useTranslations("settings.mailAccounts.grants");
  return (
    <div className="flex flex-wrap gap-x-5 gap-y-2">
      {GRANT_KEYS.map((key) => {
        const blockReason = principal ? grantBlockReason(principal, key) : null;
        return (
          <span key={key} className="flex items-center gap-1.5">
            <Switch
              id={`${idPrefix}-${key}`}
              checked={value[key]}
              disabled={readOnly || blockReason !== null}
              onCheckedChange={(checked) => onChange(applyGrantChange(value, key, checked))}
              title={
                blockReason === "agent"
                  ? t("blockAgent")
                  : blockReason === "role"
                    ? t("blockRole")
                    : undefined
              }
            />
            <Label htmlFor={`${idPrefix}-${key}`} className="text-xs font-normal">
              {t(`keys.${key}`)}
            </Label>
          </span>
        );
      })}
    </div>
  );
}

/** 신규 grant — PUT If-None-Match:* 로만 생성한다 (B.3.2). */
function NewGrantForm({
  accountId,
  candidates,
  onSaved,
}: {
  accountId: string;
  candidates: Principal[];
  onSaved: () => void;
}) {
  const t = useTranslations("settings.mailAccounts.grants");
  const tSettings = useTranslations("settings");
  const [principalId, setPrincipalId] = useState("");
  const [value, setValue] = useState<GrantBooleans>(DEFAULT_GRANT);
  const idemRef = useRef(idempotencyKey());

  const principal = candidates.find((p) => p.id === principalId);

  const create = useMutation({
    mutationFn: () =>
      putMailAccountGrant(accountId, principalId, value, {
        create: true,
        idempotencyKey: idemRef.current,
      }),
    onSuccess: () => {
      toast.success(t("saveSuccess"));
      idemRef.current = idempotencyKey();
      setPrincipalId("");
      setValue(DEFAULT_GRANT);
      onSaved();
    },
    onError: (err) => {
      toast.error(apiErrorText(tSettings, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
      onSaved();
    },
  });

  if (candidates.length === 0) return null;

  return (
    <div className="flex flex-col gap-2 rounded-2xl border border-dashed border-border p-3">
      <p className="text-sm font-medium">{t("addTitle")}</p>
      <div className="grid gap-1.5">
        <Label htmlFor="new-grant-principal">{t("principal")}</Label>
        <Select value={principalId} onValueChange={setPrincipalId}>
          <SelectTrigger id="new-grant-principal" className="w-full">
            <SelectValue placeholder={t("principalPlaceholder")} />
          </SelectTrigger>
          <SelectContent>
            {candidates.map((p) => (
              <SelectItem key={p.id} value={p.id}>
                {p.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {principal ? (
        <>
          <GrantToggles
            value={value}
            principal={principal}
            readOnly={false}
            idPrefix="new-grant"
            onChange={setValue}
          />
          <div>
            <Button
              type="button"
              size="sm"
              disabled={create.isPending}
              onClick={() => create.mutate()}
            >
              {t("addSubmit")}
            </Button>
          </div>
        </>
      ) : null}
    </div>
  );
}
