import { invoke, Channel } from "@tauri-apps/api/core";
import { $, escapeHtml, highlightRanges } from "./util";

type LineHit = { line: number; text: string; ranges: [number, number][] };
type SearchEvent =
  | { kind: "file"; path: string; hits: LineHit[] }
  | { kind: "done"; files: number; hits: number; ms: number; truncated: boolean };

/** Workspace-wide content search; results stream in from parallel Rust workers. */
export function setupSearch(opts: { onOpen: (path: string, line: number) => void }) {
  const input = $<HTMLInputElement>("#search-input");
  const caseBtn = $("#opt-case");
  const regexBtn = $("#opt-regex");
  const status = $("#search-status");
  const results = $("#search-results");
  let channel: Channel<SearchEvent> | null = null;
  let timer: number | undefined;

  function run() {
    const query = input.value;
    const ch = new Channel<SearchEvent>();
    channel = ch;
    results.replaceChildren();
    status.textContent = query ? "Searching…" : "";

    // Batch DOM appends to one per frame; results can arrive by the hundreds.
    let queue: Extract<SearchEvent, { kind: "file" }>[] = [];
    let raf = 0;
    const flush = () => {
      raf = 0;
      if (ch !== channel) return;
      results.insertAdjacentHTML("beforeend", queue.map(renderGroup).join(""));
      queue = [];
    };
    ch.onmessage = (ev) => {
      if (ch !== channel) return;
      if (ev.kind === "file") {
        queue.push(ev);
        raf ||= requestAnimationFrame(flush);
      } else {
        flush();
        status.textContent =
          `${ev.hits} result${ev.hits === 1 ? "" : "s"} in ${ev.files} file${ev.files === 1 ? "" : "s"} · ${ev.ms} ms` +
          (ev.truncated ? " (truncated)" : "");
      }
    };
    invoke("search_content", {
      query,
      regex: regexBtn.classList.contains("on"),
      caseSensitive: caseBtn.classList.contains("on"),
      onEvent: ch,
    }).catch((e) => (status.textContent = String(e)));
  }

  function renderGroup(ev: Extract<SearchEvent, { kind: "file" }>): string {
    const path = escapeHtml(ev.path);
    const rows = ev.hits
      .map(
        (h) =>
          `<div class="hit" data-path="${path}" data-line="${h.line}"><span class="ln">${h.line}</span>${highlightRanges(h.text, h.ranges)}</div>`,
      )
      .join("");
    return `<div class="group"><div class="ghead" title="${path}">${path}<span class="count">${ev.hits.length}</span></div>${rows}</div>`;
  }

  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = window.setTimeout(run, 120);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      clearTimeout(timer);
      run();
    }
  });
  for (const btn of [caseBtn, regexBtn]) {
    btn.addEventListener("click", () => {
      btn.classList.toggle("on");
      if (input.value) run();
    });
  }
  results.addEventListener("click", (e) => {
    const t = e.target as HTMLElement;
    const hit = t.closest<HTMLElement>(".hit");
    if (hit) {
      results.querySelector(".hit.sel")?.classList.remove("sel");
      hit.classList.add("sel");
      opts.onOpen(hit.dataset.path!, Number(hit.dataset.line));
    } else t.closest(".group")?.classList.toggle("collapsed");
  });

  return {
    focus(seed?: string) {
      if (seed && !seed.includes("\n")) {
        input.value = seed;
        run();
      }
      input.select();
      input.focus();
    },
    rerun() {
      if (input.value) run();
    },
  };
}
