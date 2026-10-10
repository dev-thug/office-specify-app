import { NextIntlClientProvider } from "next-intl";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { VersionEditor } from "../version-editor";
import common from "../../../../messages/en/common.json";
import templates from "../../../../messages/en/templates.json";

function editor() {
  return render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
    <VersionEditor templateId="template-1" open onOpenChange={vi.fn()} onCreated={vi.fn()} />
  </NextIntlClientProvider>);
}
beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => vi.unstubAllGlobals());

describe("version authoring", () => {
  it("deduplicates preset definitions and saves HTML plus derived text without sample defaults", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-1",version_no:1,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    editor();
    fireEvent.change(screen.getByLabelText("Subject", {exact:true}), {target:{value:"Greeting"}});
    fireEvent.click(screen.getByRole("tab", {name:"HTML code"}));
    const html = screen.getByRole("textbox", {name:"HTML code"});
    fireEvent.focus(html);
    fireEvent.click(screen.getByRole("button", {name:"Insert Contact name"}));
    fireEvent.click(screen.getByRole("button", {name:"Insert Contact name"}));
    fireEvent.change(html, {target:{value:'<p>Hello <strong>{{contact.name}}</strong></p>'}});
    expect(screen.getByLabelText("Body (text)")).toHaveValue("Hello {{contact.name}}");
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.body_html).toBe('<p>Hello <strong>{{contact.name}}</strong></p>');
    expect(request.body_text).toBe('Hello {{contact.name}}');
    expect(Object.keys(request.variable_schema.properties)).toEqual(['contact.name']);
    expect(request.variable_schema.properties['contact.name']).not.toHaveProperty('default');
    expect(JSON.stringify(request)).not.toContain('Jina Kim');
  });
  it("blocks unsafe source from saving and removes it from recipient preview", async () => {
    const fetcher = vi.fn();vi.stubGlobal("fetch",fetcher);
    editor();
    fireEvent.change(screen.getByLabelText("Subject", {exact:true}), {target:{value:"Unsafe"}});
    fireEvent.click(screen.getByRole("tab", {name:"HTML code"}));
    fireEvent.change(screen.getByRole("textbox", {name:"HTML code"}), {target:{value:'<p>Safe</p><script>bad()</script><img src="https://tracker.test">'}});
    fireEvent.click(screen.getByRole("tab", {name:"Recipient preview"}));
    const frame = screen.getByTitle("Recipient email preview");
    expect(frame).toHaveAttribute('sandbox','');
    expect(frame.getAttribute('srcdoc')).not.toContain('<script>');
    expect(frame.getAttribute('srcdoc')).not.toContain('tracker.test');
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("seeds an imported Zoho copy into a new immutable version with variables registered", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-import",version_no:3,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor templateId="template-1" open onOpenChange={vi.fn()} onCreated={vi.fn()}
        initialSeed={{subject:"Zoho welcome",bodyText:"Hello {{contact.name}}",bodyHtml:"<p>Hello {{contact.name}}</p>"}} />
    </NextIntlClientProvider>);

    await waitFor(() => expect(screen.getByLabelText("Subject", {exact:true})).toHaveValue("Zoho welcome"));
    expect(screen.getByLabelText("Body (text)")).toHaveValue("Hello {{contact.name}}");
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.body_html).toBe("<p>Hello {{contact.name}}</p>");
    expect(request.variable_schema.properties["contact.name"]).toMatchObject({type:"string"});
  });

  it("returns to an empty normal editor after a seeded import is cancelled", async () => {
    const onOpenChange = vi.fn();
    const props = {templateId:"template-1",onOpenChange,onCreated:vi.fn()};
    const view = render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor {...props} open initialSeed={{subject:"Imported",bodyText:"Imported text",bodyHtml:"<p>Imported</p>"}} />
    </NextIntlClientProvider>);
    await waitFor(() => expect(screen.getByLabelText("Subject", {exact:true})).toHaveValue("Imported"));
    view.rerender(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor {...props} open={false} initialSeed={null} />
    </NextIntlClientProvider>);
    view.rerender(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor {...props} open initialSeed={null} />
    </NextIntlClientProvider>);

    await waitFor(() => expect(screen.getByLabelText("Subject", {exact:true})).toHaveValue(""));
    expect(screen.getByLabelText("Body (text)")).toHaveValue("");
  });
});


describe("meeting booking CTA", () => {
  it("adds the server booking link as a required URI variable once and keeps the Office unsubscribe variable", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-cta",version_no:1,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor templateId="template-1" open onOpenChange={vi.fn()} onCreated={vi.fn()}
        initialSeed={{
          subject:"Meeting invitation",
          bodyText:"Manage your email preferences with {{compliance.unsubscribe_url}}",
          bodyHtml:'<p><a href="{{compliance.unsubscribe_url}}">Unsubscribe</a></p>',
        }} />
    </NextIntlClientProvider>);
    fireEvent.click(screen.getByRole("tab", {name:"HTML code"}));
    const html = screen.getByRole("textbox", {name:"HTML code"});
    fireEvent.focus(html);
    fireEvent.click(screen.getByRole("button", {name:"Insert meeting booking button"}));
    fireEvent.click(screen.getByRole("button", {name:"Insert meeting booking button"}));
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));

    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.body_html.match(/href="{{meeting\.booking_url}}"/g)).toHaveLength(2);
    expect(request.body_html).toContain('href="{{compliance.unsubscribe_url}}"');
    expect(request.body_html).not.toMatch(/https?:\/\//);
    expect(request.body_text).toContain("Book a meeting ({{meeting.booking_url}})");
    expect(request.variable_schema.properties["meeting.booking_url"]).toMatchObject({
      type:"string", format:"uri", maxLength:2048,
    });
    expect(request.variable_schema.required).toEqual(["compliance.unsubscribe_url", "meeting.booking_url"]);
  });
});
