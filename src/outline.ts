import { $ } from "./util";

export type Heading = { level: number; text: string; line: number };

/** Strip inline markdown so headings read as plain text. */
function plain(s: string): string {
  return s
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*_`~]/g, "")
    .trim();
}

/** ATX (`# x`) and setext (`x\n===`) headings, skipping fenced code and YAML front matter. */
export function parseHeadings(doc: string): Heading[] {
  const lines = doc.split("\n");
  const out: Heading[] = [];
  let i = 0;
  if (lines[0] === "---") {
    i = lines.findIndex((l, k) => k > 0 && (l === "---" || l === "...")) + 1;
  }
  let fence: string | null = null;
  for (; i < lines.length; i++) {
    const l = lines[i];
    const f = /^ {0,3}(`{3,}|~{3,})/.exec(l);
    if (fence) {
      if (f && f[1][0] === fence[0] && f[1].length >= fence.length && /^ {0,3}[`~]+\s*$/.test(l)) fence = null;
      continue;
    }
    if (f) {
      fence = f[1];
      continue;
    }
    const atx = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/.exec(l);
    if (atx) {
      out.push({ level: atx[1].length, text: plain(atx[2] ?? ""), line: i + 1 });
      continue;
    }
    const prev = lines[i - 1];
    if (
      i > 0 &&
      /^ {0,3}(=+|-+)[ \t]*$/.test(l) &&
      prev.trim() &&
      !/^\s*([-*+]\s|\d+[.)]\s|#|>|\|)/.test(prev) &&
      !out.some((h) => h.line === i)
    ) {
      out.push({ level: l.trim()[0] === "=" ? 1 : 2, text: plain(prev), line: i });
    }
  }
  return out;
}

/** Sidebar outline; highlights the section currently at the top of the view. */
export class Outline {
  private el = $("#outline");
  private headings: Heading[] = [];
  private activeLine = -1;

  constructor(onGo: (line: number) => void) {
    this.el.addEventListener("click", (e) => {
      const row = (e.target as HTMLElement).closest<HTMLElement>(".row");
      if (row) onGo(Number(row.dataset.line));
    });
  }

  get items(): Heading[] {
    return this.headings;
  }

  update(doc: string | null) {
    this.headings = doc === null ? [] : parseHeadings(doc);
    const min = Math.min(...this.headings.map((h) => h.level));
    const frag = document.createDocumentFragment();
    for (const h of this.headings) {
      const row = document.createElement("div");
      row.className = `row h${h.level}`;
      row.dataset.line = String(h.line);
      row.style.paddingLeft = `${10 + (h.level - min) * 14}px`;
      row.textContent = h.text || "(empty heading)";
      row.title = h.text;
      frag.append(row);
    }
    if (!this.headings.length) {
      const empty = document.createElement("div");
      empty.className = "empty";
      empty.textContent = doc === null ? "" : "No headings";
      frag.append(empty);
    }
    this.el.replaceChildren(frag);
    this.activeLine = -1;
  }

  /** Mark the last heading at or above `line`. */
  setTopLine(line: number) {
    let cur = -1;
    for (const h of this.headings) {
      if (h.line <= line + 0.5) cur = h.line;
      else break;
    }
    if (cur === this.activeLine) return;
    this.activeLine = cur;
    this.el.querySelector(".row.active")?.classList.remove("active");
    const row = this.el.querySelector<HTMLElement>(`.row[data-line="${cur}"]`);
    row?.classList.add("active");
    row?.scrollIntoView({ block: "nearest" });
  }
}
