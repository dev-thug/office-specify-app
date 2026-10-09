"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useRouter, useSearchParams } from "next/navigation";
import { useEffect, useRef, useState, type FormEvent } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { login } from "@/lib/api/auth";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import { safeNextPath } from "@/lib/safe-next";

export function LoginForm() {
  const t = useTranslations("auth.login");
  const tAuth = useTranslations("auth");
  const router = useRouter();
  const queryClient = useQueryClient();
  const params = useSearchParams();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hydrated, setHydrated] = useState(false);
  // 멱등 키는 논리 작업당 한 번 발급 — 확정 응답(성공/재시도 불가 오류) 후에만 재발급한다.
  const idemRef = useRef<string>(idempotencyKey());

  useEffect(() => {
    const timer = window.setTimeout(() => setHydrated(true), 0);
    return () => window.clearTimeout(timer);
  }, []);

  async function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await queryClient.cancelQueries();
      queryClient.clear();
      await login({ email, password }, { idempotencyKey: idemRef.current });
      router.push(safeNextPath(params.get("next")));
      router.refresh();
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 401 || apiErr?.code === "INVALID_CREDENTIALS")
        setError(t("invalidCredentials"));
      else if (apiErr?.status === 429 || apiErr?.code === "ACCOUNT_LOCKED")
        setError(t("accountLocked"));
      else if (apiErr?.code === "CSRF_TOKEN_INVALID")
        setError(t("csrfInvalid"));
      else setError(t("genericError"));
      // 확정 응답이면 다음 시도는 새로운 논리 작업 — 키를 재발급한다.
      // 네트워크 실패·retryable 오류는 같은 키를 유지해 재시도 중복을 막는다.
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
      setPending(false);
    }
  }

  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-4">
      <div className="grid gap-1.5">
        <Label htmlFor="login-email">{tAuth("email")}</Label>
        <Input
          id="login-email"
          type="email"
          autoComplete="email"
          required
          disabled={!hydrated}
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="login-password">{tAuth("password")}</Label>
        <Input
          id="login-password"
          type="password"
          autoComplete="current-password"
          required
          disabled={!hydrated}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </div>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <Button type="submit" className="w-full" disabled={pending || !hydrated}>
        {pending ? t("submitting") : t("submit")}
      </Button>
    </form>
  );
}
