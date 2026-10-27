import { $, escapeHtml, highlightRanges, indicesToRanges } from "./util";

export type PaletteItem = { label: string; detail?: string; labelIdx?: number[]; detailIdx?: number[]; indent?: number };

export type Provider<T extends PaletteItem> = {
  placeholder: string;
  items(query: string): T[] | Promise<T[]>;
  choose(item: T): void;
};

/**
 * One overlay, three uses: quick open (Ctrl+P), go to heading (Ctrl+Shift+O),
 * and a single-line prompt for new/rename file names.
 */
export function setupPalette() {
  const overlay = $("#palette");
  const input = $<HTMLInputElement>("#palette-input");
  const list = $("#palette-list");
  let provider: Provider<PaletteItem> | null = null;
  let items: PaletteItem[] = [];
  let sel = 0;
  let seq = 0;
  let answer: ((v: string | null) => void) | null = null;

  async function update() {
    if (!provider) return;
    const my = ++seq;
    const next = await provider.items(input.value.trim());
    if (my !== seq) return;
    items = next;
    sel = 0;
    render();
  }

  function render() {
    list.innerHTML = items
      .map((it, i) => {
        const label = highlightRanges(it.label, indicesToRanges(it.labelIdx ?? []));
        const detail = it.detail ? `<span class="dir">${highlightRanges(it.detail, indicesToRanges(it.detailIdx ?? []))}</span>` : "";
        const pad = it.indent ? ` style="padding-left:${14 + it.indent * 14}px"` : "";
        return `<li data-i="${i}" class="${i === sel ? "sel" : ""}"${pad}><span class="name">${label}</span>${detail}</li>`;
      })
      .join("");
    if (!items.length) list.innerHTML = `<li class="empty">${escapeHtml(input.value ? "No matches" : "Nothing here")}</li>`;
    list.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
  }

  function close(result: string | null = null) {
    overlay.hidden = true;
    provider = null;
    const done = answer;
    answer = null;
    done?.(result);
  }

  function choose(i: number) {
    const p = provider;
    const it = items[i];
    if (!p || !it) return;
    close();
    p.choose(it);
  }

  function show(placeholder: string, value: string) {
    if (answer) close();
    overlay.hidden = false;
    list.hidden = false;
    input.placeholder = placeholder;
    input.value = value;
    input.focus();
  }

  input.addEventListener("input", update);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") return close();
    if (e.key === "Enter") {
      e.preventDefault();
      if (answer) close(input.value.trim() || null);
      else choose(sel);
    } else if ((e.key === "ArrowDown" || e.key === "ArrowUp") && items.length) {
      e.preventDefault();
      sel = (sel + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
      render();
    }
  });
  list.addEventListener("mousedown", (e) => {
    const li = (e.target as HTMLElement).closest<HTMLElement>("li[data-i]");
    if (li) {
      e.preventDefault();
      choose(Number(li.dataset.i));
    }
  });
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });

  return {
    open<T extends PaletteItem>(p: Provider<T>) {
      show(p.placeholder, "");
      provider = p as Provider<PaletteItem>;
      update();
    },
    /** Ask for one line of text; `select` is the [start, end) range pre-selected in `value`. */
    ask(placeholder: string, value: string, select: [number, number] = [0, value.length]): Promise<string | null> {
      show(placeholder, value);
      list.hidden = true;
      items = [];
      input.setSelectionRange(select[0], select[1]);
      return new Promise((resolve) => (answer = resolve));
    },
  };
}

/** Subsequence match; returns matched indices (null if no match) and a score favoring contiguous, early hits. */
export function fuzzy(query: string, text: string): { score: number; idx: number[] } | null {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  const idx: number[] = [];
  let score = 0;
  let from = 0;
  for (const ch of q) {
    if (ch === " ") continue;
    const i = t.indexOf(ch, from);
    if (i < 0) return null;
    score += i === from ? 3 : 1;
    if (i === 0 || /[\s\-_/.]/.test(t[i - 1])) score += 2;
    idx.push(i);
    from = i + 1;
  }
  return { score: score - t.length * 0.01, idx };
}
