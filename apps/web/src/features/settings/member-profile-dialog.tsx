"use client";

import { useRef, useState, type FormEvent } from "react";
import { useTranslations } from "next-intl";
import { useQueryClient } from "@tanstack/react-query";
import { CameraIcon } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { idempotencyKey } from "@/lib/api/client";
import {
  deletePrincipalAvatar,
  principalAvatarUrl,
  updatePrincipal,
  uploadPrincipalAvatar,
  type Principal,
} from "@/lib/api/settings";
import { apiErrorText } from "@/features/settings/api-error";

const AVATAR_SIZE = 256;

/** 프로필 사진 또는 이름 첫 글자. */
export function PrincipalAvatar({ principal, className }: { principal: Pick<Principal, "id" | "name" | "avatar_updated_at">; className?: string }) {
  const url = principalAvatarUrl(principal);
  return (
    <span className={cn("flex size-8 shrink-0 items-center justify-center overflow-hidden rounded-full bg-muted text-xs font-semibold text-muted-foreground", className)}>
      {url ? (
        // eslint-disable-next-line @next/next/no-img-element -- same-origin API image behind the session
        <img src={url} alt="" className="size-full object-cover" />
      ) : (
        principal.name.trim().charAt(0).toUpperCase() || "?"
      )}
    </span>
  );
}

/** 가운데를 정사각형으로 잘라 256px로 줄인다 — 서버 한도(512KB) 안에 넉넉히 든다. */
async function squareImage(file: File): Promise<Blob> {
  const bitmap = await createImageBitmap(file);
  const side = Math.min(bitmap.width, bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = AVATAR_SIZE;
  canvas.height = AVATAR_SIZE;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("canvas unavailable");
  context.drawImage(bitmap, (bitmap.width - side) / 2, (bitmap.height - side) / 2, side, side, 0, 0, AVATAR_SIZE, AVATAR_SIZE);
  bitmap.close();
  const toBlob = (type: string) => new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, type, 0.9));
  const blob = (await toBlob("image/webp")) ?? (await toBlob("image/jpeg"));
  if (!blob) throw new Error("encode failed");
  return blob;
}

export function MemberProfileDialog({
  principal,
  canRename,
  onOpenChange,
}: {
  principal: Principal | null;
  canRename: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("settings.members.profile");
  const tSettings = useTranslations("settings");
  const queryClient = useQueryClient();
  const fileRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState(principal?.name ?? "");
  const [title, setTitle] = useState(principal?.title ?? "");
  const [photoPending, setPhotoPending] = useState(false);
  const [savePending, setSavePending] = useState(false);
  const [preview, setPreview] = useState<Pick<Principal, "id" | "name" | "avatar_updated_at"> | null>(null);

  if (!principal) return null;
  const shown = preview ?? principal;
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["settings", "principals"] });

  async function pickPhoto(file: File | undefined) {
    if (!file || !principal) return;
    if (!file.type.startsWith("image/")) {
      toast.error(t("photoInvalid"));
      return;
    }
    setPhotoPending(true);
    try {
      const image = await squareImage(file);
      const result = await uploadPrincipalAvatar(principal.id, image);
      setPreview({ ...principal, avatar_updated_at: result.data.avatar_updated_at });
      toast.success(t("photoSaved"));
      await refresh();
    } catch (error) {
      toast.error(error instanceof Error && error.message === "encode failed" ? t("photoInvalid") : apiErrorText(tSettings, error));
    } finally {
      setPhotoPending(false);
      if (fileRef.current) fileRef.current.value = "";
    }
  }

  async function removePhoto() {
    if (!principal) return;
    setPhotoPending(true);
    try {
      await deletePrincipalAvatar(principal.id);
      setPreview({ ...principal, avatar_updated_at: null });
      await refresh();
    } catch (error) {
      toast.error(apiErrorText(tSettings, error));
    } finally {
      setPhotoPending(false);
    }
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!principal) return;
    const next = name.trim();
    const nextTitle = title.trim();
    const changes = {
      ...(next && next !== principal.name ? { name: next } : {}),
      ...(nextTitle !== (principal.title ?? "") ? { title: nextTitle } : {}),
    };
    if (!canRename || !next || Object.keys(changes).length === 0) {
      onOpenChange(false);
      return;
    }
    setSavePending(true);
    try {
      await updatePrincipal(principal.id, changes, { etag: principal.version, idempotencyKey: idempotencyKey() });
      toast.success(t("nameSaved"));
      await refresh();
      onOpenChange(false);
    } catch (error) {
      toast.error(apiErrorText(tSettings, error));
      await refresh();
    } finally {
      setSavePending(false);
    }
  }

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>
        <form className="grid gap-5" onSubmit={(event) => void save(event)}>
          <div className="flex items-center gap-4">
            <button
              type="button"
              className="group relative rounded-full focus-visible:outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50"
              onClick={() => fileRef.current?.click()}
              disabled={photoPending}
              aria-label={t("changePhoto")}
            >
              <PrincipalAvatar principal={shown} className="size-20 text-2xl" />
              <span className="absolute right-0 bottom-0 flex size-7 items-center justify-center rounded-full border-2 border-background bg-primary text-primary-foreground">
                <CameraIcon className="size-3.5" aria-hidden />
              </span>
            </button>
            <div className="flex flex-col gap-1.5">
              <div className="flex flex-wrap gap-2">
                <Button type="button" size="sm" variant="outline" disabled={photoPending} onClick={() => fileRef.current?.click()}>
                  {photoPending ? t("photoUploading") : t("changePhoto")}
                </Button>
                {shown.avatar_updated_at ? (
                  <Button type="button" size="sm" variant="ghost" disabled={photoPending} onClick={() => void removePhoto()}>
                    {t("removePhoto")}
                  </Button>
                ) : null}
              </div>
              <p className="text-2xs text-muted-foreground">{t("photoHint")}</p>
            </div>
            <input ref={fileRef} type="file" accept="image/png,image/jpeg,image/webp" className="hidden" onChange={(event) => void pickPhoto(event.target.files?.[0])} />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="member-profile-name">{t("name")}</Label>
            <Input id="member-profile-name" value={name} maxLength={100} disabled={!canRename} onChange={(event) => setName(event.target.value)} />
            <p className="text-2xs text-muted-foreground">{canRename ? t("nameHint") : t("nameAdminOnly")}</p>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="member-profile-title">{t("jobTitle")}</Label>
            <Input id="member-profile-title" value={title} maxLength={100} disabled={!canRename} placeholder={t("jobTitlePlaceholder")} onChange={(event) => setTitle(event.target.value)} />
            <p className="text-2xs text-muted-foreground">{t("jobTitleHint")}</p>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>{t("close")}</Button>
            {canRename ? <Button type="submit" disabled={savePending || !name.trim()}>{savePending ? t("saving") : t("save")}</Button> : null}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
