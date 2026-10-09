"use client";

import { useQuery } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useParams, useRouter } from "next/navigation";
import { useState, type FormEvent } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { acceptInvitation, getInvitation } from "@/lib/api/auth";

export function InviteForm() {
  const t = useTranslations("auth.invite");
  const tAuth = useTranslations("auth");
  const { token } = useParams<{ token: string }>();
  const router = useRouter();
  const [displayName, setDisplayName] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const preview = useQuery({
    queryKey: ["invitation", token],
    queryFn: () => getInvitation(token),
    retry: false,
  });

  if (preview.isPending) {
    return (
      <div className="flex flex-col gap-3" aria-busy="true">
        <p className="text-sm text-muted-foreground">{t("loading")}</p>
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    );
  }

  if (preview.isError || !preview.data) {
    return (
      <p role="alert" className="text-sm text-destructive">
        {t("invalid")}
      </p>
    );
  }

  // ApiResult<InvitationPreview>.data 가 실제 미리보기 페이로드다.
  const invite = preview.data.data;

  async function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await acceptInvitation(token, {
        display_name: displayName,
        password,
      });
      router.push("/app/dashboard");
      router.refresh();
    } catch {
      setError(t("error"));
      setPending(false);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        {t("description", { email: invite.email })}
      </p>
      <dl className="flex gap-4 text-xs text-muted-foreground">
        <div>
          <dt className="inline">{t("roleLabel")}: </dt>
          <dd className="inline font-medium text-foreground">{invite.role}</dd>
        </div>
        {invite.expires_at ? (
          <div>
            <dt className="inline">{t("expiresAt")}: </dt>
            <dd className="inline font-medium text-foreground">
              {new Date(invite.expires_at).toLocaleDateString()}
            </dd>
          </div>
        ) : null}
      </dl>
      <form onSubmit={onSubmit} className="flex flex-col gap-4" noValidate>
        <div className="grid gap-1.5">
          <Label htmlFor="invite-name">{tAuth("displayName")}</Label>
          <Input
            id="invite-name"
            autoComplete="name"
            required
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
          />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="invite-password">{tAuth("password")}</Label>
          <Input
            id="invite-password"
            type="password"
            autoComplete="new-password"
            required
            minLength={12}
            maxLength={128}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            {tAuth("passwordPolicy")}
          </p>
        </div>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <Button type="submit" className="w-full" disabled={pending}>
          {pending ? t("accepting") : t("accept")}
        </Button>
      </form>
    </div>
  );
}
