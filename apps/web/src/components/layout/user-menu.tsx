"use client";

import { useMutation, useQuery } from "@tanstack/react-query";
import { LogOutIcon, UserRoundIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import { useRouter } from "next/navigation";

import { getMe, logout } from "@/lib/api/auth";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Button } from "@/components/ui/button";

function initials(name: string | undefined): string {
  if (!name) return "?";
  return name
    .split(/\s+/)
    .map((part) => part[0])
    .filter(Boolean)
    .slice(0, 2)
    .join("")
    .toUpperCase();
}

export function UserMenu() {
  const t = useTranslations("common");
  const router = useRouter();
  // 세션 정보 — API가 아직 없으면 조용히 플레이스홀더를 보인다.
  const { data } = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });
  const name = data?.display_name;

  const logoutMutation = useMutation({
    mutationFn: logout,
    onSettled: () => {
      // 세션 쿠키는 HttpOnly — 서버가 지우므로 결과와 무관하게 로그인으로 보낸다.
      router.push("/login");
      router.refresh();
    },
  });

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          className="h-9 gap-2 px-2"
          aria-label={t("accountMenu")}
        >
          <Avatar className="size-7">
            <AvatarFallback className="text-2xs font-medium">
              {initials(name)}
            </AvatarFallback>
          </Avatar>
          <span className="hidden max-w-32 truncate text-sm font-medium lg:inline">
            {name ?? "—"}
          </span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuLabel className="flex items-center gap-2">
          <UserRoundIcon className="size-4 text-muted-foreground" />
          <span className="truncate">{name ?? t("unknownUser")}</span>
        </DropdownMenuLabel>
        {data?.role ? (
          <DropdownMenuLabel className="pt-0 text-xs font-normal text-muted-foreground">
            {data.role}
          </DropdownMenuLabel>
        ) : null}
        <DropdownMenuSeparator />
        <DropdownMenuItem
          variant="destructive"
          disabled={logoutMutation.isPending}
          onSelect={() => logoutMutation.mutate()}
        >
          <LogOutIcon />
          {t("logout")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
