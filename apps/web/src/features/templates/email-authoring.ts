/** Browser-side email canvas rules. Server validation remains authoritative. */
export const DEFAULT_EMAIL_VARIABLES = [
  { name: "company.name", label: "company", type: "string", maxLength: "300", system: false },
  { name: "contact.name", label: "contact", type: "string", maxLength: "300", system: false },
  { name: "sender.display_name", label: "sender", type: "string", maxLength: "300", system: false },
  { name: "meeting.booking_url", label: "booking", type: "uri", maxLength: "2048", system: true },
  { name: "compliance.unsubscribe_url", label: "unsubscribe", type: "uri", maxLength: "2048", system: true },
] as const;

const TAGS = new Set(["p", "div", "br", "strong", "b", "em", "i", "u", "s", "ul", "ol", "li", "a", "blockquote", "h1", "h2", "h3", "hr", "table", "thead", "tbody", "tfoot", "tr", "td", "th"]);
const DROP = new Set(["script", "style", "img", "picture", "iframe", "object", "embed", "svg", "math", "form", "input", "button", "textarea", "select", "meta", "link", "base", "audio", "video", "source", "template"]);
export function safeEmailLink(value: string): boolean {
  const href = value.trim();
  if (/[\u0000-\u001f\u007f]/.test(href)) return false;
  return /^(https?:\/\/|mailto:)/i.test(href) || /^\{\{\s*(meeting\.booking_url|compliance\.unsubscribe_url)\s*\}\}$/.test(href);
}

/** An inert template keeps untrusted images/scripts out of the live document. */
export function cleanEmailHtml(raw: string): { html: string; removed: boolean } {
  const source = document.createElement("template");
  source.innerHTML = raw;
  const target = document.createElement("div");
  let removed = false;
  function copy(node: Node, into: Node) {
    if (node.nodeType === Node.TEXT_NODE) {
      into.appendChild(document.createTextNode((node.textContent ?? "").replaceAll("\u00a0", " ").replaceAll("\u200b", "")));
      return;
    }
    if (!(node instanceof Element)) return;
    const tag = node.tagName.toLowerCase();
    if (DROP.has(tag)) { removed = true; return; }
    if (!TAGS.has(tag)) {
      removed = true;
      node.childNodes.forEach(child => copy(child, into));
      return;
    }
    const element = document.createElement(tag);
    for (const attribute of node.attributes) {
      const { name, value } = attribute;
      if (tag === "a" && name === "href" && safeEmailLink(value)) {
        element.setAttribute("href", value.trim());
      } else if (tag === "a" && name === "title") {
        element.setAttribute("title", value);
      } else if (tag === "table" && name === "role" && value === "presentation") {
        element.setAttribute("role", value);
      } else if (
        tag === "table" &&
        (name === "border" || name === "cellpadding" || name === "cellspacing") &&
        /^\d+$/.test(value)
      ) {
        const max = name === "border" ? 1 : name === "cellpadding" ? 32 : 16;
        if (Number(value) >= 0 && Number(value) <= max) {
          element.setAttribute(name, value);
        } else {
          removed = true;
        }
      } else if (
        (tag === "td" || tag === "th") &&
        name === "bgcolor" &&
        /^#[0-9a-fA-F]{6}$/.test(value)
      ) {
        element.setAttribute("bgcolor", value);
      } else {
        removed = true;
      }
    }
    node.childNodes.forEach(child => copy(child, element));
    into.appendChild(element);
  }
  source.content.childNodes.forEach(node => copy(node, target));
  return { html: target.innerHTML, removed };
}

export function emailHtmlToText(raw: string): string {
  const source = document.createElement("template");
  source.innerHTML = cleanEmailHtml(raw).html;
  function text(node: Node): string {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent ?? "";
    const children = Array.from(node.childNodes).map(text).join("");
    if (!(node instanceof Element)) return children;
    const tag = node.tagName.toLowerCase();
    if (tag === "br") return "\n";
    if (tag === "a" && node.getAttribute("href") && node.getAttribute("href") !== children.trim()) return children + " (" + node.getAttribute("href") + ")";
    if (tag === "li") return "• " + children.trim() + "\n";
    if (["p", "div", "blockquote", "h1", "h2", "h3", "ul", "ol", "table"].includes(tag)) return children + "\n\n";
    if (tag === "tr") return children + "\n";
    if (["td", "th"].includes(tag)) return children + "\t";
    return children;
  }
  return text(source.content).replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
}

export function escapeEmailText(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
}
export function renderSampleEmail(html: string, values: Record<string, string>): string {
  return cleanEmailHtml(html).html.replace(/\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}/g,
    (placeholder, name: string) => Object.hasOwn(values, name) ? escapeEmailText(values[name]!) : placeholder);
}
export function emailPreviewDocument(html: string): string {
  const content = document.createElement("template");
  content.innerHTML = html;
  content.content.querySelectorAll("a").forEach(link => link.removeAttribute("href"));
  return '<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src &#39;none&#39;; style-src &#39;unsafe-inline&#39;"><meta name="referrer" content="no-referrer"><style>body{margin:0;padding:28px;font:15px/1.7 Arial,sans-serif;color-scheme:light;color:CanvasText;background:Canvas;overflow-wrap:anywhere}p{margin:0 0 16px}a{color:LinkText}table{border-collapse:collapse;max-width:100%}td,th{padding:6px}blockquote{border-left:3px solid GrayText;padding-left:12px;margin-left:0}</style></head><body>' + content.innerHTML + '</body></html>';
}
