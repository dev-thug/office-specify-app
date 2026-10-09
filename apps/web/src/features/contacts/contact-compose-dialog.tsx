"use client";

import { useQuery } from "@tanstack/react-query";
import Link from "next/link";
import { useTranslations } from "next-intl";
import { useRef, useState, type FormEvent } from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { DraftSendingWorkflow } from "@/features/sending/draft-sending-workflow";
import { idempotencyKey } from "@/lib/api/client";
import { getMe, type MeData } from "@/lib/api/auth";
import {
  createMessageDraft,
  messageDraftId,
  type MessageDraftResponse,
} from "@/lib/api/sending";
import { ApiError } from "@/lib/api/problem";

export interface ContactComposeRecipient {
  contactName: string;
  emailAddressId: string;
  address: string;
}

type MessagePurpose = "correspondence" | "marketing" | "transactional";

const PURPOSES: readonly MessagePurpose[] = [
  "correspondence",
  "marketing",
  "transactional",
];

export function ContactComposeDialog({
  open,
  onOpenChange,
  recipient,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  recipient: ContactComposeRecipient;
}) {
  const t = useTranslations("contacts.compose");
  const me = useQuery<MeData>({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    enabled: open,
    retry: false,
    staleTime: 60_000,
  });
  const mailAccounts = (me.data?.mail_accounts ?? []).filter((account) =>
    Array.isArray(account.allowed_actions) &&
    account.allowed_actions.includes("mail.compose")
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>
            {t("description", {
              name: recipient.contactName,
              address: recipient.address,
            })}
          </DialogDescription>
        </DialogHeader>
        {me.isPending ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("mailboxLoading")}
          </p>
        ) : me.isError ? (
          <div className="grid gap-3">
            <p role="alert" className="text-sm text-destructive">
              {t("mailboxError")}
            </p>
            <div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => void me.refetch()}
              >
                {t("mailboxRetry")}
              </Button>
            </div>
          </div>
        ) : mailAccounts.length === 0 ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t("noMailbox")}
          </p>
        ) : (
          <ComposeDraftForm
            recipient={recipient}
            mailAccounts={mailAccounts}
            onClose={() => onOpenChange(false)}
            t={t}
          />
        )}
      </DialogContent>
    </Dialog>
  );
}

function ComposeDraftForm({
  recipient,
  mailAccounts,
  onClose,
  t,
}: {
  recipient: ContactComposeRecipient;
  mailAccounts: MeData["mail_accounts"];
  onClose: () => void;
  t: ReturnType<typeof useTranslations>;
}) {
  const [mailAccountId, setMailAccountId] = useState("");
  const [purpose, setPurpose] = useState<MessagePurpose | "">("");
  const [subject, setSubject] = useState("");
  const [bodyText, setBodyText] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [createdDraft, setCreatedDraft] =
    useState<MessageDraftResponse | null>(null);
  const operationKey = useRef<string | null>(null);

  const selectedMailbox = mailAccounts.find(
    (account) => account.id === mailAccountId
  );
  const canCreate =
    selectedMailbox?.allowed_actions.includes("mail.compose") === true &&
    purpose !== "" &&
    subject.trim().length > 0 &&
    bodyText.trim().length > 0 &&
    !pending;

  function changed<T>(setter: (value: T) => void, value: T) {
    setter(value);
    operationKey.current = null;
    setError(null);
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canCreate || !selectedMailbox) return;
    operationKey.current ??= idempotencyKey();
    setPending(true);
    setError(null);
    try {
      const result = await createMessageDraft(
        {
          mail_account_id: selectedMailbox.id,
          to_email_address_id: recipient.emailAddressId,
          purpose,
          subject: subject.trim(),
          body_text: bodyText,
        },
        { idempotencyKey: operationKey.current }
      );
      if (!messageDraftId(result.data)) {
        setError(t("error"));
        return;
      }
      setCreatedDraft(result.data);
      operationKey.current = null;
    } catch (cause) {
      setError(t("error"));
      if (
        cause instanceof ApiError &&
        cause.problem.retryable !== true &&
        cause.status < 500
      ) {
        operationKey.current = null;
      }
    } finally {
      setPending(false);
    }
  }

  if (createdDraft) {
    const draftId = messageDraftId(createdDraft);
    return draftId ? (
      <>
        <DraftSendingWorkflow
          draftId={draftId}
          mailAccountId={createdDraft.mail_account_id}
        />
        <div className="flex justify-end">
          <Button variant="link" size="sm" asChild>
            <Link href={`/app/inbox/drafts/${encodeURIComponent(draftId)}`}>
              {t("openDraft")}
            </Link>
          </Button>
        </div>
      </>
    ) : null;
  }

  return (
    <form className="grid gap-4" onSubmit={(event) => void submit(event)}>
      <div className="grid gap-1.5">
        <span className="text-xs font-medium text-muted-foreground">
          {t("recipientLabel")}
        </span>
        <p className="rounded-2xl border border-border bg-muted/30 px-3 py-2.5 text-sm">
          <span className="font-medium">{recipient.contactName}</span>{" "}
          <span className="text-muted-foreground">&lt;{recipient.address}&gt;</span>
        </p>
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="contact-compose-mailbox">{t("mailboxLabel")}</Label>
        <Select
          value={mailAccountId}
          onValueChange={(value) => changed(setMailAccountId, value)}
        >
          <SelectTrigger
            id="contact-compose-mailbox"
            aria-label={t("mailboxLabel")}
            className="w-full"
          >
            <SelectValue placeholder={t("mailboxPlaceholder")} />
          </SelectTrigger>
          <SelectContent>
            {mailAccounts.map((account) => (
              <SelectItem key={account.id} value={account.id}>
                {account.address}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="contact-compose-purpose">{t("purposeLabel")}</Label>
        <Select
          value={purpose}
          onValueChange={(value) => changed(setPurpose, value as MessagePurpose)}
        >
          <SelectTrigger
            id="contact-compose-purpose"
            aria-label={t("purposeLabel")}
            className="w-full"
          >
            <SelectValue placeholder={t("purposePlaceholder")} />
          </SelectTrigger>
          <SelectContent>
            {PURPOSES.map((value) => (
              <SelectItem key={value} value={value}>
                {t(`purpose.${value}` as never)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="contact-compose-subject">{t("subject")}</Label>
        <Input
          id="contact-compose-subject"
          required
          maxLength={998}
          value={subject}
          onChange={(event) => changed(setSubject, event.target.value)}
          placeholder={t("subjectPlaceholder")}
        />
      </div>
      <div className="grid gap-1.5">
        <Label htmlFor="contact-compose-message">{t("message")}</Label>
        <Textarea
          id="contact-compose-message"
          required
          value={bodyText}
          onChange={(event) => changed(setBodyText, event.target.value)}
          placeholder={t("messagePlaceholder")}
          className="min-h-36"
        />
      </div>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onClose}>
          {t("cancel")}
        </Button>
        <Button type="submit" disabled={!canCreate}>
          {pending ? t("submitting") : t("submit")}
        </Button>
      </DialogFooter>
    </form>
  );
}
