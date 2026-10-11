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
  it("edits a saved version inline, preserves schema and saves a new version", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"v2",version_no:2}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    const onCreated = vi.fn();
    const variableSchema = {type:"object" as const, properties:{custom:{type:"string",maxLength:60,default:"Guest"}, unused:{type:"string",format:"uri"}},required:[],additionalProperties:false as const};
    render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor templateId="template-1" open inline onOpenChange={vi.fn()} onCreated={onCreated}
        initialSeed={{subject:"Saved subject",bodyText:"Hello {{custom}}",bodyHtml:"<p>Hello {{custom}}</p>",variableSchema}} />
    </NextIntlClientProvider>);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    const canvas = screen.getByRole("textbox", {name:"Email message"});
    canvas.innerHTML = "<p>Updated {{custom}}</p>";
    fireEvent.input(canvas);
    fireEvent.click(screen.getByRole("button", {name:"Edit HTML"}));
    expect(screen.getByRole("textbox", {name:"HTML code"})).toHaveValue("<p>Updated {{custom}}</p>");
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(onCreated).toHaveBeenCalledWith("v2"));
    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.variable_schema).toEqual(variableSchema);
    expect(request.body_text).toBe("Updated {{custom}}");
    expect(screen.getByLabelText("Subject", {exact:true})).toHaveValue("Saved subject");
  });

  it("deduplicates preset definitions and saves HTML plus derived text without sample defaults", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-1",version_no:1,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    editor();
    fireEvent.change(screen.getByLabelText("Subject", {exact:true}), {target:{value:"Greeting"}});
    fireEvent.click(screen.getByRole("button", {name:"Edit HTML"}));
    const html = screen.getByRole("textbox", {name:"HTML code"});
    fireEvent.focus(html);
    fireEvent.click(screen.getByRole("button", {name:"Insert Contact name"}));
    fireEvent.click(screen.getByRole("button", {name:"Insert Contact name"}));
    fireEvent.change(html, {target:{value:'<p>Hello <strong>{{contact.name}}</strong></p>'}});
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
    fireEvent.click(screen.getByRole("button", {name:"Edit HTML"}));
    fireEvent.change(screen.getByRole("textbox", {name:"HTML code"}), {target:{value:'<p>Safe</p><script>bad()</script><img src="https://tracker.test">'}});
    fireEvent.click(screen.getByRole("button", {name:"Recipient preview"}));
    const frame = screen.getByTitle("Recipient email preview");
    expect(frame).toHaveAttribute('sandbox','');
    expect(frame.getAttribute('srcdoc')).not.toContain('<script>');
    expect(frame.getAttribute('srcdoc')).not.toContain('tracker.test');
    fireEvent.click(screen.getByRole("button", {name:"Close"}));
    await waitFor(() => expect(screen.getByRole("button", {name:"Save version"})).toBeInTheDocument());
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
    expect(screen.getByRole("textbox", {name:"Email message"}).innerHTML).toBe("");
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
    fireEvent.click(screen.getByRole("button", {name:"Edit HTML"}));
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


describe("unified email editor", () => {
  it("keeps rendered and raw HTML edits in one pane and derives saved plain text", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-unified",version_no:1,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    editor();

    expect(screen.queryByRole("textbox", {name:"Body (text)"})).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Subject", {exact:true}), {target:{value:"Unified draft"}});
    const canvas = screen.getByRole("textbox", {name:"Email message"});
    canvas.innerHTML = "<p>First draft</p>";
    fireEvent.input(canvas);

    fireEvent.click(screen.getByRole("button", {name:"Edit HTML"}));
    const source = screen.getByRole("textbox", {name:"HTML code"});
    expect(source).toHaveValue("<p>First draft</p>");
    fireEvent.change(source, {target:{value:"<p>Final draft</p>"}});
    fireEvent.click(screen.getByRole("button", {name:"Back to email canvas"}));
    expect(screen.getByRole("textbox", {name:"Email message"}).innerHTML).toBe("<p>Final draft</p>");

    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.body_html).toBe("<p>Final draft</p>");
    expect(request.body_text).toBe("Final draft");
  });

  it("converts a plaintext-only seed into the editable email canvas before saving", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-text-seed",version_no:1,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor templateId="template-1" open onOpenChange={vi.fn()} onCreated={vi.fn()}
        initialSeed={{subject:"Plain seed",bodyText:"\n  Hello\n\n\nWorld  \n",bodyHtml:null}} />
    </NextIntlClientProvider>);

    expect(screen.getByRole("textbox", {name:"Email message"}).innerHTML).toBe("<p><br>  Hello</p><p>World  <br></p>");
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.body_html).toBe("<p><br>  Hello</p><p>World  <br></p>");
    expect(request.body_text).toBe("\n  Hello\n\n\nWorld  \n");
  });

  it("shows and preserves legacy manual text alongside HTML until the canvas is edited", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({data:{template_version_id:"version-legacy",version_no:1,content_hash:"hash"}}), {status:201,headers:{"content-type":"application/json"}}));
    vi.stubGlobal("fetch", fetcher);
    render(<NextIntlClientProvider locale="en" messages={{common, templates}}>
      <VersionEditor templateId="template-1" open onOpenChange={vi.fn()} onCreated={vi.fn()}
        initialSeed={{subject:"Legacy seed",bodyText:"Plain copy preserved for old clients",bodyHtml:"<p>Rich copy</p>"}} />
    </NextIntlClientProvider>);

    expect(screen.getByText("The original plain text is being preserved. Editing the email canvas will generate new plain text from the HTML.")).toBeInTheDocument();
    expect(screen.getByText("Plain copy preserved for old clients")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", {name:"Save version"}));
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    const request = JSON.parse(fetcher.mock.calls[0]![1]!.body as string);
    expect(request.body_text).toBe("Plain copy preserved for old clients");
  });
});
