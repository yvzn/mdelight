export const $ = <T extends HTMLElement = HTMLElement>(sel: string) => document.querySelector(sel) as T;

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

/** Wrap `ranges` ([start, end) offsets into `text`) in <mark>. */
export function highlightRanges(text: string, ranges: [number, number][]): string {
  let out = "";
  let pos = 0;
  for (const [s, e] of ranges) {
    if (s < pos) continue;
    out += escapeHtml(text.slice(pos, s)) + "<mark>" + escapeHtml(text.slice(s, e)) + "</mark>";
    pos = e;
  }
  return out + escapeHtml(text.slice(pos));
}

/** Collapse sorted single-char indices into ranges. */
export function indicesToRanges(indices: number[], offset = 0): [number, number][] {
  const out: [number, number][] = [];
  for (const raw of indices) {
    const i = raw - offset;
    if (i < 0) continue;
    const last = out[out.length - 1];
    if (last && last[1] === i) last[1] = i + 1;
    else out.push([i, i + 1]);
  }
  return out;
}

export const basename = (p: string) => p.slice(Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")) + 1);
export const dirname = (p: string) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "");
export const isMarkdown = (p: string) => /\.(md|markdown|mdx|mdown|mkd)$/i.test(p);

/** Resolve `href` against workspace-relative dir `baseDir`; null if it escapes the workspace. */
export function resolveRel(baseDir: string, href: string): string | null {
  const parts = href.startsWith("/") || !baseDir ? [] : baseDir.split("/");
  for (const seg of href.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      if (!parts.length) return null;
      parts.pop();
    } else parts.push(seg);
  }
  return parts.join("/");
}

export const store = {
  get(key: string): string | null {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  set(key: string, value: string) {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* storage unavailable: not critical */
    }
  },
};

let toastTimer: number | undefined;
export function toast(msg: string) {
  const el = $("#toast");
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), 4000);
}
