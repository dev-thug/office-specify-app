const BLOCKED_ELEMENTS = [
  "script",
  "meta",
  "base",
  "link",
  "iframe",
  "frame",
  "frameset",
  "object",
  "embed",
  "applet",
  "portal",
  "fencedframe",
  "form",
  "input",
  "button",
  "textarea",
  "select",
  "option",
  "fieldset",
  "source",
  "track",
  "audio",
  "video",
  "template",
  "svg",
  "math",
].join(",");

const URL_ATTRIBUTES = new Set([
  "href",
  "src",
  "srcset",
  "imagesrcset",
  "poster",
  "background",
  "action",
  "formaction",
  "cite",
  "data",
  "xlink:href",
  "longdesc",
  "manifest",
  "ping",
  "codebase",
  "archive",
]);

const INLINE_RASTER_IMAGE = /^data:image\/(?:png|gif|jpe?g|webp|avif);base64,[a-z0-9+/]+=*$/i;
const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f]/;

function isInlineRasterImage(value: string): boolean {
  return INLINE_RASTER_IMAGE.test(value.trim());
}

/** Preserve normal inline CSS, but remove imports and any CSS URL that could load a resource. */
function sanitizeNativeCss(css: string): string {
  return css
    .replace(/@import\b[^;]*(?:;|$)/gi, "")
    .replace(/url\(\s*(?:"([^"]*)"|'([^']*)'|([^)]*?))\s*\)/gi, (_match, doubleQuoted: string | undefined, singleQuoted: string | undefined, bare: string | undefined) => {
      const url = (doubleQuoted ?? singleQuoted ?? bare ?? "").trim();
      if (url.startsWith("#") && !CONTROL_CHARACTERS.test(url)) return `url("${url}")`;
      return isInlineRasterImage(url) ? `url("${url}")` : "none";
    });
}

/**
 * Produce an inert srcDoc that keeps a Zoho template's native inline layout while
 * preventing active content and network loads. Office import still uses the much
 * smaller cleanEmailHtml allowlist; this function is only for isolated preview.
 */
export function createZohoNativePreviewDocument(source: string): string {
  // HTMLTemplateElement.content is inert: unlike DOMParser's returned document,
  // parsing raw remote markup here cannot start image or frame requests first.
  const inert = document.createElement("template");
  inert.innerHTML = source;
  const fragment = inert.content;
  fragment.querySelectorAll(BLOCKED_ELEMENTS).forEach((element) => element.remove());

  fragment.querySelectorAll("style").forEach((style) => {
    style.textContent = sanitizeNativeCss(style.textContent ?? "");
  });

  fragment.querySelectorAll("*").forEach((element) => {
    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      const value = attribute.value;
      if (name.startsWith("on") || name === "srcdoc" || name === "nonce") {
        element.removeAttribute(attribute.name);
      } else if (name === "style") {
        element.setAttribute(attribute.name, sanitizeNativeCss(value));
      } else if (URL_ATTRIBUTES.has(name)) {
        const tag = element.tagName.toLowerCase();
        if (name === "href" && tag === "a" && value.trim().startsWith("#") && !CONTROL_CHARACTERS.test(value)) {
          element.setAttribute(attribute.name, value.trim());
        } else if (name === "src" && tag === "img" && isInlineRasterImage(value)) {
          element.setAttribute(attribute.name, value.trim());
        } else {
          element.removeAttribute(attribute.name);
        }
      }
    }
  });

  const output = document.implementation.createHTMLDocument("");
  output.head.replaceChildren();
  output.body.replaceChildren();

  const csp = output.createElement("meta");
  csp.setAttribute("http-equiv", "Content-Security-Policy");
  csp.setAttribute(
    "content",
    "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src 'none'; media-src 'none'; connect-src 'none'; frame-src 'none'; child-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"
  );
  output.head.append(csp);

  const title = fragment.querySelector("title");
  if (title) output.title = title.textContent ?? "";
  fragment.querySelectorAll("style").forEach((style) => output.head.append(style));

  const bodySource = fragment.querySelector("body") ?? fragment.querySelector("html") ?? fragment;
  function appendContent(node: ParentNode, parent: Node) {
    for (const child of Array.from(node.childNodes)) {
      if (child.nodeType === Node.ELEMENT_NODE && ["html", "head", "body"].includes((child as Element).tagName.toLowerCase())) {
        appendContent(child as Element, parent);
      } else {
        parent.appendChild(child);
      }
    }
  }
  appendContent(bodySource, output.body);

  return `<!doctype html>${output.documentElement.outerHTML}`;
}
