import morphdom from "morphdom";
import { convertFileSrc } from "@tauri-apps/api/core";
import { LanguageDescription, type Language } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { classHighlighter, highlightCode } from "@lezer/highlight";
import type { Mermaid } from "mermaid";
import { isDark } from "./theme";
import { dirname, escapeHtml, resolveRel } from "./util";

type Rendered = { svg: string } | { error: string };

const cache = new Map<string, Rendered>();
let mermaidPromise: Promise<Mermaid> | null = null;
let renderId = 0;

function initMermaid(m: Mermaid): Mermaid {
  m.initialize({ startOnLoad: false, securityLevel: "strict", theme: isDark() ? "dark" : "default" });
  return m;
}

function loadMermaid(): Promise<Mermaid> {
  return (mermaidPromise ??= import("mermaid").then(({ default: m }) => initMermaid(m)));
}

/**
 * Theme changed: forget rendered diagrams so the next preview pass redraws them.
 * Old SVGs stay on screen until their replacements are ready, so nothing flashes.
 */
export function resetDiagrams() {
  cache.clear();
  if (mermaidPromise) mermaidPromise = mermaidPromise.then(initMermaid);
}

function hash(s: string): string {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h * 33) ^ s.charCodeAt(i)) >>> 0;
  return h.toString(36) + s.length.toString(36);
}

// ---------------------------------------------------------------- code highlighting
// Reuses the editor's lezer parsers, so no extra highlighter ships with the app.

/** Fence name → parser once loaded; null when the language is unknown. */
const langs = new Map<string, Language | null>();
const highlighted = new Map<string, string>();

async function loadLanguage(name: string) {
  const desc = LanguageDescription.matchLanguageName(languages, name, true);
  try {
    langs.set(name, desc ? (await desc.load()).language : null);
  } catch {
    langs.set(name, null);
  }
}

function highlight(name: string, lang: Language, code: string): string {
  const key = name + ":" + hash(code);
  let html = highlighted.get(key);
  if (html === undefined) {
    html = "";
    highlightCode(
      code,
      lang.parser.parse(code),
      classHighlighter,
      (text, cls) => (html += cls ? `<span class="${cls}">${escapeHtml(text)}</span>` : escapeHtml(text)),
      () => (html += "\n"),
    );
    highlighted.set(key, html);
    if (highlighted.size > 500) highlighted.delete(highlighted.keys().next().value!);
  }
  return html;
}

function absPath(root: string, rel: string): string {
  return root.replace(/[\\/]+$/, "") + "\\" + rel.replace(/\//g, "\\");
}

/** Diagram source by key, so the copy button can hand back the code behind an SVG. */
const mermaidSrc = new Map<string, string>();

function button(cls: string, label: string, title: string): HTMLButtonElement {
  const b = document.createElement("button");
  b.className = cls;
  b.type = "button";
  b.textContent = label;
  b.title = title;
  return b;
}

/** Hover toolbar for a wide block: fullscreen, plus Copy for code and diagrams. */
function blockTools(copy: boolean): HTMLElement {
  const div = document.createElement("div");
  div.className = "tools";
  div.append(button("expand", "⛶", "Open fullscreen (or double-click)"));
  if (copy) div.append(button("copy", "Copy", "Copy to clipboard"));
  return div;
}

/** Text a preview copy button refers to: the code block's code or the diagram's source. */
export function codeBlockText(btn: HTMLElement): string {
  const diagram = btn.closest<HTMLElement>(".mermaid");
  if (diagram) return mermaidSrc.get(diagram.dataset.key ?? "") ?? "";
  return btn.closest("pre")?.querySelector("code")?.textContent ?? "";
}

function fill(div: HTMLElement, r: Rendered) {
  div.classList.remove("pending");
  if ("svg" in r) {
    div.classList.remove("has-error");
    div.innerHTML = r.svg;
  } else if (div.querySelector("svg")) {
    // Keep the last good diagram while the user is mid-edit; flag the error.
    div.classList.add("has-error");
    div.dataset.error = r.error;
    return;
  } else {
    div.innerHTML = `<pre class="mermaid-error">${escapeHtml(r.error)}</pre>`;
  }
  div.append(blockTools(true));
}

/**
 * Patch `el` with freshly rendered HTML. Unchanged nodes (and already-rendered
 * diagrams) are left alone, so scroll position and images don't flicker.
 * Resolves true when new highlighting languages were loaded and a re-render would add colors.
 */
export async function renderPreview(el: HTMLElement, html: string, ctx: { root: string; file: string }): Promise<boolean> {
  // <template> content is inert: images don't load until nodes reach the live DOM.
  const tpl = document.createElement("template");
  tpl.innerHTML = `<div>${html}</div>`;
  const next = tpl.content.firstElementChild as HTMLElement;
  const dir = dirname(ctx.file);

  for (const img of next.querySelectorAll("img")) {
    const src = img.getAttribute("src") ?? "";
    if (!src || src.startsWith("//") || /^[a-z][a-z0-9+.-]*:/i.test(src)) continue;
    let rel: string | null = null;
    try {
      rel = resolveRel(dir, decodeURIComponent(src.split(/[?#]/)[0]));
    } catch {
      /* malformed URI */
    }
    if (rel !== null) img.setAttribute("src", convertFileSrc(absPath(ctx.root, rel)));
  }

  const pending: [string, string][] = [];
  for (const code of next.querySelectorAll("pre > code.language-mermaid")) {
    const pre = code.parentElement!;
    const src = code.textContent ?? "";
    const key = hash(src);
    mermaidSrc.set(key, src);
    if (mermaidSrc.size > 300) mermaidSrc.delete(mermaidSrc.keys().next().value!);
    const div = document.createElement("div");
    div.className = "mermaid";
    div.dataset.key = key;
    const sp = pre.getAttribute("data-sourcepos");
    if (sp) div.setAttribute("data-sourcepos", sp);
    const hit = cache.get(key);
    if (hit) fill(div, hit);
    else {
      div.classList.add("pending");
      div.textContent = src;
      pending.push([key, src]);
    }
    pre.replaceWith(div);
  }

  const toLoad = new Set<string>();
  for (const code of next.querySelectorAll<HTMLElement>("pre > code[class^='language-']")) {
    const name = code.className.slice("language-".length).toLowerCase();
    const lang = langs.get(name);
    if (lang) code.innerHTML = highlight(name, lang, code.textContent ?? "");
    else if (lang === undefined) toLoad.add(name);
  }
  for (const pre of next.querySelectorAll("pre:has(> code)")) pre.append(blockTools(true));
  // Top-level tables get a wrapper: it carries the tools and scrolls when the table is wider than the pane.
  for (const table of next.querySelectorAll(":scope > table")) {
    const wrap = document.createElement("div");
    wrap.className = "table-wrap";
    table.replaceWith(wrap);
    wrap.append(table, blockTools(false));
  }
  // A paragraph holding only an image (optionally linked) is a figure: wide, centered, zoomable.
  for (const p of next.querySelectorAll(":scope > p:has(img)")) {
    if (p.textContent?.trim()) continue;
    p.classList.add("figure");
    p.append(blockTools(false));
  }

  morphdom(el, next, {
    childrenOnly: true,
    onBeforeElUpdated(from, to) {
      if (from.isEqualNode(to)) return false;
      if (from.classList.contains("mermaid") && to.classList.contains("mermaid") && to.classList.contains("pending")) {
        // Keep showing the old diagram until the new one is ready.
        from.dataset.key = to.dataset.key;
        from.classList.add("pending");
        if (to.dataset.sourcepos) from.dataset.sourcepos = to.dataset.sourcepos;
        return !from.querySelector("svg");
      }
      return true;
    },
  });

  const langsLoaded = toLoad.size ? Promise.all([...toLoad].map(loadLanguage)).then(() => true) : Promise.resolve(false);
  if (!pending.length) return langsLoaded;
  const mermaid = await loadMermaid();
  for (const [key, src] of pending) {
    let r = cache.get(key);
    if (!r) {
      try {
        r = { svg: (await mermaid.render(`mmd-${++renderId}`, src)).svg };
      } catch (e) {
        r = { error: String((e as Error)?.message ?? e) };
      }
      cache.set(key, r);
      if (cache.size > 300) cache.delete(cache.keys().next().value!);
    }
    for (const div of el.querySelectorAll<HTMLElement>(`.mermaid.pending[data-key="${key}"]`)) fill(div, r);
  }
  return langsLoaded;
}

// ---------------------------------------------------------------- scroll sync

export type Anchor = { line: number; top: number };

const BLOCKS = "[data-sourcepos]:is(p,h1,h2,h3,h4,h5,h6,li,pre,blockquote,table,tr,hr,div)";

/** Map source lines to vertical offsets inside the scrollable preview. */
export function computeAnchors(el: HTMLElement, totalLines: number): Anchor[] {
  const base = el.getBoundingClientRect().top - el.scrollTop;
  const out: Anchor[] = [{ line: 1, top: 0 }];
  for (const n of el.querySelectorAll<HTMLElement>(BLOCKS)) {
    const line = parseInt(n.dataset.sourcepos!, 10);
    const top = n.getBoundingClientRect().top - base;
    const last = out[out.length - 1];
    if (line > last.line && top > last.top) out.push({ line, top });
  }
  const last = out[out.length - 1];
  out.push({ line: Math.max(totalLines + 1, last.line + 1), top: Math.max(el.scrollHeight, last.top + 1) });
  return out;
}

function interpolate(anchors: Anchor[], value: number, from: keyof Anchor, to: keyof Anchor): number {
  let lo = 0;
  let hi = anchors.length - 1;
  while (lo < hi - 1) {
    const mid = (lo + hi) >> 1;
    if (anchors[mid][from] <= value) lo = mid;
    else hi = mid;
  }
  const a = anchors[lo];
  const b = anchors[hi];
  const t = Math.max(0, Math.min(1, (value - a[from]) / (b[from] - a[from] || 1)));
  return a[to] + t * (b[to] - a[to]);
}

export const lineToTop = (anchors: Anchor[], line: number) => interpolate(anchors, line, "line", "top");
export const topToLine = (anchors: Anchor[], top: number) => interpolate(anchors, top, "top", "line");
