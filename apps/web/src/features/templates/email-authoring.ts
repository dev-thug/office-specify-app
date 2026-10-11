/** Browser-side email canvas rules. Server validation remains authoritative. */
export const DEFAULT_EMAIL_VARIABLES = [
  { name: "company.name", label: "company", type: "string", maxLength: "300", system: false },
  { name: "contact.name", label: "contact", type: "string", maxLength: "300", system: false },
  { name: "sender.display_name", label: "sender", type: "string", maxLength: "300", system: false },
  // 회사마다 다른 본문 — 에이전트가 조사 내용으로 작성해 채운다 (줄바꿈 유지).
  { name: "message.body", label: "personalBody", type: "string", maxLength: "5000", system: false },
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

const EMAIL_STYLE_PROPERTIES = new Set([
  "color",
  "background-color",
  "font-size",
  "font-weight",
  "font-family",
  "text-align",
  "line-height",
  "padding",
  "margin",
  "border",
  "border-radius",
  "width",
  "max-width",
  "text-decoration",
  "display",
  "border-collapse",
  "border-spacing",
  "vertical-align",
  "table-layout",
]);
const EMAIL_FONT_FAMILIES = new Set([
  "arial", "apple sd gothic neo", "courier new", "georgia", "helvetica", "lato",
  "malgun gothic", "monospace", "noto sans", "noto sans kr", "open sans",
  "roboto", "sans-serif", "serif", "segoe ui", "system-ui", "tahoma",
  "times new roman", "trebuchet ms", "verdana",
]);
const EMAIL_COLOR_NAMES = new Set([
  "aqua", "black", "blue", "brown", "fuchsia", "gray", "green", "lime",
  "maroon", "navy", "olive", "orange", "purple", "rebeccapurple", "red",
  "silver", "teal", "transparent", "white", "yellow",
]);

/** Port of the backend email CSS subset; unknown or ambiguous declarations are removed. */
function sanitizeEmailInlineStyle(style: string): string | null {
  if (style.trim().length === 0) return "";
  if (
    /[^\x00-\x7f]/.test(style) ||
    /[\u0000-\u001f\u007f\\/*@<>!]/.test(style)
  ) {
    return null;
  }

  const rawDeclarations = style.split(";");
  const declarations: string[] = [];
  const seen = new Set<string>();
  for (let index = 0; index < rawDeclarations.length; index += 1) {
    const declaration = rawDeclarations[index]!.trim();
    if (declaration.length === 0) {
      if (index === rawDeclarations.length - 1 && style.trimEnd().endsWith(";")) {
        continue;
      }
      return null;
    }
    const separator = declaration.indexOf(":");
    if (separator < 0) return null;
    const property = declaration.slice(0, separator).trim();
    const value = declaration.slice(separator + 1).trim();
    if (
      !EMAIL_STYLE_PROPERTIES.has(property) ||
      seen.has(property) ||
      !safeEmailStyleValue(property, value)
    ) {
      return null;
    }
    seen.add(property);
    declarations.push(property + ":" + value);
  }
  return declarations.join(";");
}

function safeEmailStyleValue(property: string, value: string): boolean {
  const lower = value.toLowerCase();
  switch (property) {
    case "color":
      return safeEmailTextColor(lower);
    case "background-color":
      return safeEmailColor(lower);
    case "font-size": {
      const unitStart = lower.search(/[a-z%]/);
      const split = unitStart < 0 ? lower.length : unitStart;
      const size = parseEmailDecimal(lower.slice(0, split));
      const unit = lower.slice(split);
      if (size === null) return false;
      if (unit === "px") return size >= 8 && size <= 72;
      if (unit === "pt") return size >= 6 && size <= 54;
      return false;
    }
    case "font-weight": {
      if (["normal", "bold", "bolder", "lighter"].includes(lower)) return true;
      if (!/^\d+$/.test(lower)) return false;
      const weight = Number(lower);
      return Number.isSafeInteger(weight) && weight >= 100 && weight <= 900 && weight % 100 === 0;
    }
    case "font-family":
      return lower.split(",").every((rawFamily) => {
        let family = rawFamily.trim();
        if (family.startsWith("'") && family.endsWith("'")) {
          family = family.slice(1, -1);
        } else if (family.startsWith('"') && family.endsWith('"')) {
          family = family.slice(1, -1);
        }
        family = family.split(/\s+/).filter(Boolean).join(" ");
        return EMAIL_FONT_FAMILIES.has(family);
      });
    case "text-align":
      return ["left", "right", "center", "justify", "start", "end"].includes(lower);
    case "line-height": {
      if (lower === "0") return false;
      if (lower === "normal") return true;
      const multiplier = parseEmailDecimal(lower);
      return (
        (multiplier !== null && multiplier >= 0.75 && multiplier <= 3.5) ||
        safeEmailLength(lower, 64, 300, 8)
      );
    }
    case "padding":
      return safeEmailBoxLengths(lower, false, 64, 100, 8);
    case "margin":
      return safeEmailBoxLengths(lower, true, 64, 100, 8);
    case "border":
      return safeEmailBorder(lower);
    case "border-radius":
      return safeEmailBoxLengths(lower, false, 32, 50, 8);
    case "width":
    case "max-width":
      return lower !== "0" && safeEmailLength(lower, 1200, 100, 100);
    case "text-decoration":
      return lower === "none" || lower === "underline";
    case "display":
      return ["inline", "inline-block", "block"].includes(lower);
    case "border-collapse":
      return lower === "collapse" || lower === "separate";
    case "border-spacing":
      return safeEmailBorderSpacing(lower);
    case "vertical-align":
      return ["baseline", "top", "middle", "bottom", "text-top", "text-bottom"].includes(lower);
    case "table-layout":
      return lower === "auto" || lower === "fixed";
    default:
      return false;
  }
}

function parseEmailDecimal(value: string): number | null {
  if (value.length === 0 || value.startsWith("+") || value.startsWith("-")) return null;
  let decimalPoints = 0;
  let digits = 0;
  for (const character of value) {
    if (character >= "0" && character <= "9") digits += 1;
    else if (character === ".") decimalPoints += 1;
    else return null;
  }
  if (digits === 0 || decimalPoints > 1) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function safeEmailLength(
  value: string,
  maxPx: number,
  maxPercent: number,
  maxEm: number
): boolean {
  if (value === "0") return true;
  const unitStart = value.search(/[a-z%]/);
  const split = unitStart < 0 ? value.length : unitStart;
  const number = parseEmailDecimal(value.slice(0, split));
  if (number === null || number <= 0) return false;
  const unit = value.slice(split);
  if (unit === "px") return number <= maxPx;
  if (unit === "pt") return number <= maxPx * 0.75;
  if (unit === "em" || unit === "rem") return number <= maxEm;
  if (unit === "%") return maxPercent > 0 && number <= maxPercent;
  return false;
}

function safeEmailBoxLengths(
  value: string,
  allowAuto: boolean,
  maxPx: number,
  maxPercent: number,
  maxEm: number
): boolean {
  const values = value.split(/\s+/).filter(Boolean);
  return (
    values.length >= 1 &&
    values.length <= 4 &&
    values.every((part) => (part === "auto" && allowAuto) || safeEmailLength(part, maxPx, maxPercent, maxEm))
  );
}

function safeEmailBorderSpacing(value: string): boolean {
  const values = value.split(/\s+/).filter(Boolean);
  return (
    values.length >= 1 &&
    values.length <= 2 &&
    values.every((part) => safeEmailLength(part, 32, 0, 8))
  );
}

function safeEmailBorder(value: string): boolean {
  let width = false;
  let style = false;
  let color = false;
  for (const part of value.split(/\s+/).filter(Boolean)) {
    if (!width && safeEmailLength(part, 8, 0, 1)) width = true;
    else if (!style && ["none", "solid", "dashed", "dotted", "double"].includes(part)) style = true;
    else if (!color && safeEmailSimpleColor(part)) color = true;
    else return false;
  }
  return width || style || color;
}

function safeEmailColor(value: string): boolean {
  if (safeEmailSimpleColor(value)) return true;
  const colorFunction = /^(rgb|rgba)\((.*)\)$/.exec(value);
  if (!colorFunction) return false;
  const parts = colorFunction[2]!.split(",").map((part) => part.trim());
  if (colorFunction[1] === "rgb") {
    return parts.length === 3 && parts.every(safeEmailRgbChannel);
  }
  return parts.length === 4 &&
    parts.slice(0, 3).every(safeEmailRgbChannel) &&
    safeEmailAlpha(parts[3]!);
}

function safeEmailTextColor(value: string): boolean {
  if (value === "transparent") return false;
  if (value.startsWith("#")) {
    const hex = value.slice(1);
    if (!/^[0-9a-f]+$/.test(hex)) return false;
    if (hex.length === 3 || hex.length === 6) return true;
    if (hex.length === 4) return hex[3]! >= "c";
    if (hex.length === 8) return Number.parseInt(hex.slice(6), 16) >= 0xcc;
    return false;
  }
  const rgba = /^rgba\((.*)\)$/.exec(value);
  if (rgba) {
    const parts = rgba[1]!.split(",").map((part) => part.trim());
    if (parts.length !== 4 || !parts.slice(0, 3).every(safeEmailRgbChannel)) return false;
    const alpha = parts[3]!;
    if (alpha.endsWith("%")) {
      const percent = parseEmailDecimal(alpha.slice(0, -1));
      return percent !== null && percent >= 80 && percent <= 100;
    }
    const number = parseEmailDecimal(alpha);
    return number !== null && number >= 0.8 && number <= 1;
  }
  return safeEmailColor(value);
}

function safeEmailSimpleColor(value: string): boolean {
  if (EMAIL_COLOR_NAMES.has(value)) return true;
  if (!value.startsWith("#")) return false;
  const hex = value.slice(1);
  return [3, 4, 6, 8].includes(hex.length) && /^[0-9a-f]+$/.test(hex);
}

function safeEmailRgbChannel(value: string): boolean {
  if (value.endsWith("%")) {
    const percent = parseEmailDecimal(value.slice(0, -1));
    return percent !== null && percent >= 0 && percent <= 100;
  }
  const channel = parseEmailDecimal(value);
  return channel !== null && channel >= 0 && channel <= 255;
}

function safeEmailAlpha(value: string): boolean {
  if (value.endsWith("%")) {
    const percent = parseEmailDecimal(value.slice(0, -1));
    return percent !== null && percent >= 0 && percent <= 100;
  }
  const alpha = parseEmailDecimal(value);
  return alpha !== null && alpha >= 0 && alpha <= 1;
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
      } else if (name === "style") {
        const sanitizedStyle = sanitizeEmailInlineStyle(value);
        if (sanitizedStyle === null) {
          removed = true;
        } else if (sanitizedStyle) {
          element.setAttribute("style", sanitizedStyle);
        }
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
    (placeholder, name: string) => Object.hasOwn(values, name)
      ? escapeEmailText(values[name]!).replace(/\r\n/g, "\n").replace(/\n/g, "<br>")
      : placeholder);
}
export function emailPreviewDocument(html: string): string {
  const content = document.createElement("template");
  content.innerHTML = html;
  content.content.querySelectorAll("a").forEach(link => link.removeAttribute("href"));
  return '<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src &#39;none&#39;; style-src &#39;unsafe-inline&#39;"><meta name="referrer" content="no-referrer"><style>body{margin:0;padding:28px;font:15px/1.7 Arial,sans-serif;color-scheme:light;color:CanvasText;background:Canvas;overflow-wrap:anywhere}p{margin:0 0 16px}a{color:LinkText}table{border-collapse:collapse;max-width:100%}td,th{padding:6px}blockquote{border-left:3px solid GrayText;padding-left:12px;margin-left:0}</style></head><body>' + content.innerHTML + '</body></html>';
}
