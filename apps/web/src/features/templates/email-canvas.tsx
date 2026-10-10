"use client";

import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import { useTranslations } from "next-intl";
import { BoldIcon, ItalicIcon, LinkIcon, ListIcon, ListOrderedIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import styles from "./email-canvas.module.css";
import { cleanEmailHtml, emailPreviewDocument, escapeEmailText, safeEmailLink } from "./email-authoring";

export interface EmailCanvasHandle {
  insertVariable: (name: string) => void;
  insertBookingCta: (label: string) => void;
}

export const EmailCanvas = forwardRef<EmailCanvasHandle, {
  html: string;
  previewHtml: string;
  previewSubject: string;
  onChange: (html: string) => void;
  onFocus: () => void;
}>(function EmailCanvas({ html, previewHtml, previewSubject, onChange, onFocus }, ref) {
  const t = useTranslations("templates.authoring");
  const [mode, setMode] = useState<"compose" | "html">("compose");
  const [previewOpen, setPreviewOpen] = useState(false);
  const [linkOpen, setLinkOpen] = useState(false);
  const [link, setLink] = useState("");
  const [linkError, setLinkError] = useState(false);
  const editor = useRef<HTMLDivElement>(null);
  const code = useRef<HTMLTextAreaElement>(null);
  const selection = useRef<Range | null>(null);
  const pendingInsert = useRef<{ command: "insertText" | "insertHTML"; value: string } | null>(null);
  const cleaned = typeof document === "undefined" ? {html: "", removed: false} : cleanEmailHtml(html);

  useEffect(() => {
    if (editor.current && document.activeElement !== editor.current && editor.current.innerHTML !== cleaned.html) {
      editor.current.innerHTML = cleaned.html;
      selection.current = null;
    }
  }, [cleaned.html, mode]);

  function remember() {
    const selected = window.getSelection();
    if (selected?.rangeCount && editor.current?.contains(selected.anchorNode)) {
      selection.current = selected.getRangeAt(0).cloneRange();
    }
  }
  function restore() {
    const element = editor.current;
    if (!element) return;
    element.focus();
    const selected = window.getSelection();
    if (!selected) return;
    selected.removeAllRanges();
    if (selection.current && element.contains(selection.current.commonAncestorContainer)) {
      selected.addRange(selection.current);
    } else {
      const range = document.createRange();
      range.selectNodeContents(element);
      range.collapse(false);
      selected.addRange(range);
    }
  }
  function changed() {
    if (!editor.current) return;
    onChange(cleanEmailHtml(editor.current.innerHTML).html);
    remember();
  }
  function command(name: string, value?: string) {
    restore();
    // Native editing commands preserve the browser undo stack. Disable CSS
    // output so basic formatting remains within the server's HTML policy.
    document.execCommand("styleWithCSS", false, "false");
    document.execCommand(name, false, value);
    changed();
  }
  useEffect(() => {
    if (mode === "compose" && pendingInsert.current) {
      const pending = pendingInsert.current;
      pendingInsert.current = null;
      command(pending.command, pending.value);
    }
  });
  useImperativeHandle(ref, () => ({
    insertVariable(name) {
      const value = "{{" + name + "}}";
      if (mode === "html" && code.current) {
        const element = code.current;
        const start = element.selectionStart;
        onChange(html.slice(0, start) + value + html.slice(element.selectionEnd));
        element.focus();
        requestAnimationFrame(() => element.setSelectionRange(start + value.length, start + value.length));
      } else if (mode === "compose") {
        command("insertText", value);
      } else {
        pendingInsert.current = { command: "insertText", value };
        setMode("compose");
      }
    },
    insertBookingCta(label) {
      const value = '<table role="presentation" border="0" cellpadding="12" cellspacing="0"><tbody><tr><td bgcolor="#f3f4f6"><a href="{{meeting.booking_url}}"><strong>' + escapeEmailText(label) + "</strong></a></td></tr></tbody></table>";
      if (mode === "html" && code.current) {
        const element = code.current;
        const start = element.selectionStart;
        const end = element.selectionEnd;
        onChange(html.slice(0, start) + value + html.slice(end));
        element.focus();
        const cursor = start + value.length;
        requestAnimationFrame(() => element.setSelectionRange(cursor, cursor));
      } else if (mode === "compose") {
        command("insertHTML", value);
      } else {
        pendingInsert.current = { command: "insertHTML", value };
        setMode("compose");
      }
    },
  }));

  const tools = [
    { command: "bold", label: "bold", Icon: BoldIcon },
    { command: "italic", label: "italic", Icon: ItalicIcon },
    { command: "insertUnorderedList", label: "bullets", Icon: ListIcon },
    { command: "insertOrderedList", label: "numbered", Icon: ListOrderedIcon },
  ] as const;
  return (
    <section className="overflow-hidden rounded-xl border bg-card" aria-label={t("canvas")}>
      <div className="flex flex-wrap items-center justify-between gap-2 border-b bg-muted/40 p-2">
        <div className="flex items-center gap-1">
          <Button type="button" size="sm" variant={mode === "html" ? "secondary" : "ghost"}
            aria-pressed={mode === "html"} onClick={() => setMode(current => current === "compose" ? "html" : "compose")}>
            {mode === "compose" ? t("editHtml") : t("backToCanvas")}
          </Button>
          <Button type="button" size="sm" variant="ghost" onClick={() => setPreviewOpen(true)}>{t("preview")}</Button>
        </div>
        <span className="px-2 text-2xs text-muted-foreground">{t(mode === "html" ? "htmlSourceHint" : "canvasHint")}</span>
      </div>
      <div hidden={mode !== "compose"}>
        <div role="toolbar" aria-label={t("formatting")} className="flex gap-1 border-b px-3 py-2">
          {tools.map(({command: name, label, Icon}) => (
            <Button key={name} type="button" size="icon-sm" variant="ghost" aria-label={t(label)}
              onMouseDown={event => event.preventDefault()} onClick={() => command(name)}><Icon aria-hidden /></Button>
          ))}
          <Button type="button" size="icon-sm" variant="ghost" aria-label={t("link")}
            onMouseDown={event => event.preventDefault()} onClick={() => { remember(); setLinkOpen(value => !value); }}><LinkIcon aria-hidden /></Button>
        </div>
        {linkOpen ? <div className="flex flex-wrap items-center gap-2 border-b p-3">
          <Input aria-label={t("linkUrl")} placeholder="https://" value={link} onChange={event => setLink(event.target.value)} className="min-w-0 flex-1" />
          <Button type="button" size="sm" onClick={() => {
            if (!safeEmailLink(link)) { setLinkError(true); return; }
            command("createLink", link.trim()); setLinkOpen(false); setLink(""); setLinkError(false);
          }}>{t("applyLink")}</Button>
          {linkError ? <p role="alert" className="w-full text-xs text-destructive">{t("invalidLink")}</p> : null}
        </div> : null}
        <div className="bg-muted/30 p-3 sm:p-5">
          <div ref={editor} role="textbox" aria-multiline="true" aria-label={t("body")} contentEditable suppressContentEditableWarning
            data-placeholder={t("placeholder")} onFocus={() => { onFocus(); remember(); }} onKeyUp={remember} onMouseUp={remember} onBlur={remember}
            onDrop={event => {
              event.preventDefault();
              const dropped = event.dataTransfer.getData("text/html");
              if (dropped) command("insertHTML", cleanEmailHtml(dropped).html);
              else command("insertText", event.dataTransfer.getData("text/plain"));
            }}
            onDragOver={event => event.preventDefault()}
            onInput={changed} onClick={event => { if ((event.target as Element).closest("a")) event.preventDefault(); }}
            onPaste={event => {
              event.preventDefault();
              const pasted = event.clipboardData.getData("text/html");
              if (pasted) command("insertHTML", cleanEmailHtml(pasted).html);
              else command("insertText", event.clipboardData.getData("text/plain"));
            }}
            className={styles.canvas + " min-h-64 px-6 py-5 shadow-xs outline-none empty:before:pointer-events-none empty:before:content-[attr(data-placeholder)] focus-visible:ring-2 focus-visible:ring-ring [&_p]:mb-4 [&_a]:underline [&_ul]:list-disc [&_ul]:pl-6 [&_ol]:list-decimal [&_ol]:pl-6 [&_table]:max-w-full [&_td]:p-1"} />
        </div>
      </div>
      <div hidden={mode !== "html"}>
        <Textarea ref={code} id="tv-body-html" aria-label={t("html")} value={html}
          onFocus={onFocus} onChange={event => onChange(event.target.value)} spellCheck={false}
          className="min-h-80 resize-y rounded-none border-0 font-mono text-ui shadow-none" />
      </div>
      <Dialog open={previewOpen} onOpenChange={setPreviewOpen}>
        <DialogContent className="sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>{t("preview")}</DialogTitle>
            <DialogDescription>{t("sampleOnly")}</DialogDescription>
          </DialogHeader>
          <div className="border-b px-6 py-4">
            <p className="text-2xs text-muted-foreground">{t("sampleRecipient")}</p>
            <p className="mt-1 text-sm font-medium">{previewSubject || t("noSubject")}</p>
          </div>
          <iframe title={t("previewFrame")} sandbox="" referrerPolicy="no-referrer"
            srcDoc={emailPreviewDocument(previewHtml)} className="h-80 w-full bg-background" />
        </DialogContent>
      </Dialog>
      {cleaned.removed ? <p role="alert" className="border-t px-4 py-3 text-xs text-destructive">{t("unsupportedHtml")}</p> : null}
    </section>
  );
});
