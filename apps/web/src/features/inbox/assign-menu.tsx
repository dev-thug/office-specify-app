"use client";

/**
 * 담당 배정 메뉴 — 활성 인간 principal 목록 + 나에게 배정 + 배정 해제.
 * 실제 변경은 PUT assignment + If-Match 로 부모가 처리한다.
 */
import { UserCheck } from "lucide-react";
import { useTranslations } from "next-intl";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { PrincipalSummary } from "@/lib/api/inbox";

interface AssignMenuProps {
  principals: PrincipalSummary[];
  meId: string | undefined;
  currentAssigneeId: string | null;
  disabled?: boolean;
  pending?: boolean;
  onAssign: (principalId: string | null) => void;
}

export function AssignMenu({
  principals,
  meId,
  currentAssigneeId,
  disabled,
  pending,
  onAssign,
}: AssignMenuProps) {
  const t = useTranslations("inbox.detail");
  const tc = useTranslations("common");

  // 배정은 사람에게만 — 활성 인간 principal 만 노출한다 (§5.4 can_assign).
  const humans = principals.filter(
    (p) => p.kind === "human" && p.status === "active"
  );

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled || pending}
        >
          <UserCheck aria-hidden />
          {t("assign")}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>{t("assignee")}</DropdownMenuLabel>
        {meId ? (
          <DropdownMenuItem
            disabled={currentAssigneeId === meId}
            onSelect={() => onAssign(meId)}
          >
            {t("assignToMe")}
          </DropdownMenuItem>
        ) : null}
        <DropdownMenuSeparator />
        {humans.map((p) => (
          <DropdownMenuItem
            key={p.id}
            disabled={p.id === currentAssigneeId}
            onSelect={() => onAssign(p.id)}
          >
            {p.name || tc("unknownUser")}
          </DropdownMenuItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem
          disabled={currentAssigneeId === null}
          onSelect={() => onAssign(null)}
        >
          {t("unassign")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
