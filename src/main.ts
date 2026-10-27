import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { createEditor, gotoLine, replaceDoc, scrollToLine, setDocument, topLine } from "./editor";
import { codeBlockText, computeAnchors, lineToTop, renderPreview, resetDiagrams, topToLine, type Anchor } from "./preview";
import { onThemeChange } from "./theme";
import { setupThemeMenu } from "./theme-menu";
import { FileTree } from "./tree";
import { Outline } from "./outline";
import { fuzzy, setupPalette, type PaletteItem, type Provider } from "./palette";
import { setupSearch } from "./search";
import { $, basename, dirname, escapeHtml, isMarkdown, resolveRel, store, toast } from "./util";
import "@fontsource-variable/inter";
import "./style.css";

type Mode = "code" | "split" | "preview";

const state = {
  root: null as string | null,
  files: [] as string[],
  fileSet: new Set<string>(),
  current: null as string | null,
  eol: "\n",
  /** Last text known to be on disk (LF-normalized); tells our own saves apart from external edits. */
  disk: "",
  dirty: false,
  // Every launch starts as a reader; Ctrl+1/2 switch to editing for the session.
  mode: "preview" as Mode,
  recent: [] as string[],
};

const panesEl = $("#panes");
const previewEl = $("#preview");
const editor = createEditor($("#editor"), { onChange, onScroll: onEditorScroll });
const tree = new FileTree($("#tree"), (p) => openFile(p));
const palette = setupPalette();
const search = setupSearch({ onOpen: (p, line) => openFile(p, line) });
const outline = new Outline((line) => revealLine(line, "start"));
const themeMenu = setupThemeMenu();
// Editor and CSS follow on their own; diagrams are SVGs drawn for one theme, so redraw them.
onThemeChange(() => {
  resetDiagrams();
  refreshPreview();
});

type FileItem = PaletteItem & { path: string };

function fileItem(path: string, indices: number[] = []): FileItem {
  const cut = path.lastIndexOf("/") + 1;
  return {
    path,
    label: path.slice(cut),
    detail: path.slice(0, cut),
    labelIdx: indices.filter((i) => i >= cut).map((i) => i - cut),
    detailIdx: indices.filter((i) => i < cut),
  };
}

const fileProvider: Provider<FileItem> = {
  placeholder: "Go to file…",
  async items(q) {
    if (!q) {
      const seen = new Set(state.recent);
      return [...state.recent, ...state.files.filter((f) => !seen.has(f))].slice(0, 50).map((p) => fileItem(p));
    }
    const res = await invoke<{ path: string; indices: number[] }[]>("find_files", { query: q, limit: 50 });
    return res.map((r) => fileItem(r.path, r.indices));
  },
  choose: (it) => openFile(it.path),
};

type HeadingItem = PaletteItem & { line: number };

const headingProvider: Provider<HeadingItem> = {
  placeholder: "Go to heading…",
  items(q) {
    const hs = outline.items;
    const min = Math.min(...hs.map((h) => h.level));
    const all = hs.map((h) => ({ line: h.line, label: h.text, indent: q ? 0 : h.level - min, score: 0, labelIdx: [] as number[] }));
    if (!q) return all;
    return all
      .flatMap((it) => {
        const m = fuzzy(q, it.label);
        return m ? [{ ...it, score: m.score, labelIdx: m.idx }] : [];
      })
      .sort((a, b) => b.score - a.score);
  },
  choose: (it) => revealLine(it.line, "start"),
};

// ---------------------------------------------------------------- workspace

async function pickFolder() {
  const dir = await open({ directory: true, title: "Open folder" });
  if (typeof dir === "string") openWorkspace(dir);
}

async function openWorkspace(path: string, silent = false) {
  let files: string[];
  try {
    files = await invoke<string[]>("open_workspace", { path });
  } catch (e) {
    if (!silent) toast(String(e));
    return;
  }
  await flushSave();
  state.root = path;
  state.current = null;
  state.recent = [];
  setFiles(files);
  store.set("workspace", path);
  store.set("recentWorkspaces", JSON.stringify([path, ...recentWorkspaces().filter((p) => p !== path)].slice(0, 6)));
  $("#ws-name").textContent = basename(path.replace(/[\\/]+$/, "")) || path;
  $("#ws-menu-btn").title = path;
  document.body.classList.add("has-ws");
  const last = store.get("last:" + path);
  const first = last && state.fileSet.has(last) ? last : files.find((f) => /^readme\.md$/i.test(f)) ?? files.find(isMarkdown);
  if (first) openFile(first);
  else updateTitle();
  search.rerun();
}

function recentWorkspaces(): string[] {
  try {
    return JSON.parse(store.get("recentWorkspaces") ?? "[]");
  } catch {
    return [];
  }
}

// Folder menu on the workspace name: open another folder or switch to a recent one.
const wsMenu = $("#ws-menu");
const wsMenuBtn = $("#ws-menu-btn");

function toggleWsMenu(open = wsMenu.hidden) {
  wsMenu.hidden = !open;
  wsMenuBtn.setAttribute("aria-expanded", String(open));
  if (!open) return;
  const others = recentWorkspaces().filter((p) => p !== state.root);
  wsMenu.innerHTML =
    `<button role="menuitem" data-act="open">Open folder…<kbd>Ctrl+O</kbd></button>` +
    (others.length ? `<div class="sep">Recent</div>` : "") +
    others
      .map((p) => `<button role="menuitem" data-path="${escapeHtml(p)}" title="${escapeHtml(p)}">${escapeHtml(basename(p.replace(/[\\/]+$/, "")))}<span class="dir">${escapeHtml(p)}</span></button>`)
      .join("");
  wsMenu.querySelector("button")?.focus();
}

wsMenuBtn.addEventListener("click", () => toggleWsMenu());
wsMenu.addEventListener("click", (e) => {
  const item = (e.target as HTMLElement).closest<HTMLElement>("button");
  if (!item) return;
  toggleWsMenu(false);
  if (item.dataset.act === "open") pickFolder();
  else if (item.dataset.path) openWorkspace(item.dataset.path);
});
document.addEventListener("mousedown", (e) => {
  if (!wsMenu.hidden && !(e.target as HTMLElement).closest("#ws-menu, #ws-menu-btn")) toggleWsMenu(false);
});
wsMenu.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    toggleWsMenu(false);
    wsMenuBtn.focus();
  }
});

function setFiles(files: string[]) {
  state.files = files;
  state.fileSet = new Set(files);
  tree.setFiles(files);
  if (state.current) tree.setActive(state.current);
}

let openSeq = 0;
async function openFile(path: string, line?: number) {
  if (path !== state.current) {
    const my = ++openSeq;
    await flushSave();
    let text: string;
    try {
      text = await invoke<string>("read_file", { path });
    } catch (e) {
      toast(String(e));
      return;
    }
    if (my !== openSeq) return;
    state.current = path;
    state.eol = text.includes("\r\n") ? "\r\n" : "\n";
    state.disk = text.replace(/\r\n/g, "\n");
    state.dirty = false;
    setDocument(editor, text, path);
    state.recent = [path, ...state.recent.filter((p) => p !== path)].slice(0, 30);
    if (state.root) store.set("last:" + state.root, path);
    tree.setActive(path);
    previewEl.scrollTop = 0;
    previewEl.replaceChildren();
    outline.update(text.replace(/\r\n/g, "\n"));
    applyMode();
    updateTitle();
    await refreshPreview();
    trackTop();
  }
  if (line) revealLine(line);
}

/** Jump to a source line in whichever pane is showing. */
function revealLine(line: number, y: "center" | "start" = "center") {
  if (effectiveMode() === "preview") {
    const top = lineToTop(getAnchors(), line);
    previewEl.scrollTop = Math.max(0, y === "start" ? top - 8 : top - previewEl.clientHeight / 3);
  } else gotoLine(editor, line, y);
}

// ---------------------------------------------------------------- new / rename

async function newFile() {
  if (!state.root) return;
  const dir = state.current ? dirname(state.current) : "";
  const initial = (dir ? dir + "/" : "") + "untitled.md";
  const start = initial.length - "untitled.md".length;
  let name = await palette.ask("New file path (relative to the folder)", initial, [start, start + "untitled".length]);
  if (!name) return;
  name = name.replace(/\\/g, "/").replace(/^\/+/, "");
  if (!/\.[^/.]+$/.test(name)) name += ".md";
  try {
    setFiles(await invoke<string[]>("create_file", { path: name }));
  } catch (e) {
    return toast(String(e));
  }
  await openFile(name);
  if (effectiveMode() === "preview") setMode("split");
  editor.focus();
}

/** Click the title (or F2) to edit the path in place: Enter/blur commits, Esc cancels. */
function startRename() {
  const from = state.current;
  const title = $("#doc-title");
  if (!from || title.querySelector("input")) return;
  const input = document.createElement("input");
  input.className = "rename-input";
  input.value = from;
  input.spellcheck = false;
  input.setAttribute("aria-label", "File name (a path moves the file)");
  title.replaceChildren(input);
  const cut = from.lastIndexOf("/") + 1;
  const dot = from.lastIndexOf(".");
  input.focus();
  input.setSelectionRange(cut, dot > cut ? dot : from.length);

  let done = false;
  const finish = (commit: boolean) => {
    if (done) return;
    done = true;
    const to = input.value.trim();
    title.textContent = state.current ?? "";
    if (commit && to) renameFile(from, to);
    else editor.focus();
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") finish(true);
    else if (e.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
}

async function renameFile(from: string, to: string) {
  to = to.replace(/\\/g, "/").replace(/^\/+/, "");
  if (to === from) return;
  await flushSave();
  try {
    setFiles(await invoke<string[]>("rename_file", { from, to }));
  } catch (e) {
    return toast(String(e));
  }
  if (state.current === from) {
    state.current = to;
    state.recent = state.recent.map((p) => (p === from ? to : p));
    if (state.root) store.set("last:" + state.root, to);
    tree.setActive(to);
    updateTitle();
    applyMode();
    refreshPreview();
  }
}

function updateTitle() {
  const title = $("#doc-title");
  if (!title.querySelector("input")) title.textContent = state.current ?? "";
  title.classList.toggle("editable", !!state.current);
  $("#doc-state").textContent = state.dirty ? "●" : "";
  document.title = [state.current && basename(state.current), state.root && basename(state.root), "MDelight"]
    .filter(Boolean)
    .join(" — ");
}

// ---------------------------------------------------------------- saving (autosave, no prompts)

let saveTimer: number | undefined;
let saving: Promise<unknown> = Promise.resolve();

function flushSave(): Promise<unknown> {
  clearTimeout(saveTimer);
  if (!state.dirty || !state.current) return saving;
  const path = state.current;
  let content = editor.state.doc.toString();
  state.disk = content;
  if (state.eol !== "\n") content = content.replace(/\n/g, state.eol);
  state.dirty = false;
  updateTitle();
  saving = saving
    .then(() => invoke("write_file", { path, content }))
    .catch((e) => {
      if (state.current === path) state.dirty = true;
      updateTitle();
      toast("Save failed: " + e);
    });
  return saving;
}

function onChange() {
  state.dirty = true;
  updateTitle();
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(flushSave, 800);
  schedulePreview();
  clearTimeout(outlineTimer);
  outlineTimer = window.setTimeout(() => outline.update(editor.state.doc.toString()), 200);
}

let outlineTimer: number | undefined;
let trackRaf = 0;

/** Highlight the outline entry for the section at the top of the visible pane. */
function trackTop() {
  trackRaf ||= requestAnimationFrame(() => {
    trackRaf = 0;
    const line = effectiveMode() === "preview" ? topToLine(getAnchors(), previewEl.scrollTop) : topLine(editor);
    outline.setTopLine(line);
  });
}

// ---------------------------------------------------------------- preview

let previewTimer: number | undefined;
let previewSeq = 0;
let lastRenderMs = 0;

const effectiveMode = (): Mode => (state.current && isMarkdown(state.current) ? state.mode : "code");
const previewVisible = () => effectiveMode() !== "code";

function schedulePreview() {
  if (!previewVisible()) return;
  clearTimeout(previewTimer);
  // Adaptive debounce: big documents render less often while typing.
  previewTimer = window.setTimeout(refreshPreview, Math.min(300, Math.max(16, lastRenderMs * 2)));
}

async function refreshPreview() {
  if (!state.current || !previewVisible()) return;
  const my = ++previewSeq;
  const file = state.current;
  const t0 = performance.now();
  const html = await invoke<string>("render_markdown", { text: editor.state.doc.toString() });
  if (my !== previewSeq || file !== state.current) return;
  const langsLoaded = await renderPreview(previewEl, html, { root: state.root!, file });
  lastRenderMs = performance.now() - t0;
  anchorsDirty = true;
  if (langsLoaded && my === previewSeq) refreshPreview();
}

// ---------------------------------------------------------------- fullscreen viewer

const WIDE_BLOCK = ".table-wrap, .mermaid, pre, p.figure";
const lightbox = $("#lightbox");
const lightboxBody = $(".lb-body");
let lightboxReturnFocus: HTMLElement | null = null;

function openLightbox(block: Element) {
  lightboxReturnFocus = document.activeElement as HTMLElement | null;
  lightboxBody.replaceChildren(block.cloneNode(true));
  lightbox.hidden = false;
  lightboxBody.scrollTo(0, 0);
  lightboxBody.focus();
}

function closeLightbox() {
  if (lightbox.hidden) return false;
  lightbox.hidden = true;
  lightboxBody.replaceChildren();
  lightboxReturnFocus?.focus();
  return true;
}

$("#lb-close").addEventListener("click", closeLightbox);
lightboxBody.addEventListener("click", (e) => {
  if (e.target === lightboxBody) closeLightbox();
});
// Double-click a diagram or image to enlarge it (tables and code keep double-click for selecting text).
previewEl.addEventListener("dblclick", (e) => {
  const block = (e.target as HTMLElement).closest(".mermaid, p.figure");
  if (block && !(e.target as HTMLElement).closest(".tools")) openLightbox(block);
});

previewEl.addEventListener("click", (e) => {
  const expandBtn = (e.target as HTMLElement).closest<HTMLElement>("button.expand");
  if (expandBtn) {
    const block = expandBtn.closest(WIDE_BLOCK);
    if (block) openLightbox(block);
    return;
  }
  const copyBtn = (e.target as HTMLElement).closest<HTMLElement>("button.copy");
  if (copyBtn) {
    copyText(codeBlockText(copyBtn)).then(
      () => {
        copyBtn.textContent = "Copied";
        copyBtn.classList.add("done");
        setTimeout(() => {
          copyBtn.textContent = "Copy";
          copyBtn.classList.remove("done");
        }, 1200);
      },
      (err) => toast("Copy failed: " + err),
    );
    return;
  }
  const a = (e.target as HTMLElement).closest("a");
  const href = a?.getAttribute("href");
  if (!href) return;
  e.preventDefault();
  if (href.startsWith("#")) {
    document.getElementById(decodeURIComponent(href.slice(1)))?.scrollIntoView();
  } else if (/^[a-z][a-z0-9+.-]*:/i.test(href)) {
    openUrl(href).catch((err) => toast(String(err)));
  } else if (state.current) {
    const [target] = href.split("#");
    const rel = resolveRel(dirname(state.current), decodeURIComponent(target));
    if (rel !== null && state.fileSet.has(rel)) openFile(rel);
    else toast("File not found: " + target);
  }
});

async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Clipboard API can be refused without a user-activation grant; fall back to the legacy path.
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.append(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    if (!ok) throw new Error("clipboard unavailable");
  }
}

// ---------------------------------------------------------------- scroll sync

let anchors: Anchor[] = [];
let anchorsDirty = true;
let lock: "editor" | "preview" | null = null;
let lockTimer: number | undefined;

function getAnchors(): Anchor[] {
  if (anchorsDirty) {
    anchors = computeAnchors(previewEl, editor.state.doc.lines);
    anchorsDirty = false;
  }
  return anchors;
}

function holdLock(side: "editor" | "preview") {
  lock = side;
  clearTimeout(lockTimer);
  lockTimer = window.setTimeout(() => (lock = null), 120);
}

function onEditorScroll() {
  trackTop();
  if (effectiveMode() !== "split" || lock === "preview") return;
  holdLock("editor");
  previewEl.scrollTop = lineToTop(getAnchors(), topLine(editor));
}

previewEl.addEventListener(
  "scroll",
  () => {
    if (effectiveMode() === "preview") trackTop();
    if (effectiveMode() !== "split" || lock === "editor") return;
    holdLock("preview");
    scrollToLine(editor, topToLine(getAnchors(), previewEl.scrollTop));
  },
  { passive: true },
);
// Layout shifts (resize, images loading) invalidate the line → offset map.
new ResizeObserver(() => (anchorsDirty = true)).observe(previewEl);
previewEl.addEventListener("load", () => (anchorsDirty = true), true);

// ---------------------------------------------------------------- modes & UI

function setMode(mode: Mode) {
  state.mode = mode;
  applyMode();
  refreshPreview();
}

function applyMode() {
  const mode = effectiveMode();
  panesEl.dataset.mode = mode;
  const md = !!state.current && isMarkdown(state.current);
  for (const b of document.querySelectorAll<HTMLButtonElement>(".modes button")) {
    b.classList.toggle("active", b.dataset.mode === mode);
    b.disabled = !md;
  }
  anchorsDirty = true;
  editor.requestMeasure();
}

type Panel = "files" | "search" | "outline";

function showPanel(name: Panel) {
  setSidebar(true);
  for (const b of document.querySelectorAll<HTMLElement>(".side-tabs [data-panel]")) {
    b.classList.toggle("active", b.dataset.panel === name);
  }
  $("#panel-files").hidden = name !== "files";
  $("#panel-search").hidden = name !== "search";
  $("#panel-outline").hidden = name !== "outline";
  if (name === "outline") trackTop();
}

function setSidebar(visible: boolean) {
  document.body.classList.toggle("side-hidden", !visible);
  $("#toggle-sidebar").title = `${visible ? "Hide" : "Show"} sidebar (Ctrl+B)`;
  store.set("sidebar", visible ? "1" : "0");
  editor.requestMeasure();
  anchorsDirty = true;
}

for (const b of document.querySelectorAll<HTMLElement>(".side-tabs [data-panel]")) {
  b.addEventListener("click", () => {
    const name = b.dataset.panel as Panel;
    showPanel(name);
    if (name === "search") search.focus();
  });
}
for (const b of document.querySelectorAll<HTMLElement>(".modes button")) {
  b.addEventListener("click", () => setMode(b.dataset.mode as Mode));
}
const helpEl = $("#help");
let helpReturnFocus: HTMLElement | null = null;

function toggleHelp(open = helpEl.hidden) {
  if (open === !helpEl.hidden) return;
  helpEl.hidden = !open;
  if (open) {
    helpReturnFocus = document.activeElement as HTMLElement | null;
    $(".help-box").focus();
  } else helpReturnFocus?.focus();
}

$("#help-close").addEventListener("click", () => toggleHelp(false));
helpEl.addEventListener("mousedown", (e) => {
  if (e.target === helpEl) toggleHelp(false);
});

$("#welcome-open").addEventListener("click", pickFolder);
$("#new-file").addEventListener("click", newFile);
$("#doc-title").addEventListener("click", startRename);
$("#toggle-sidebar").addEventListener("click", () => setSidebar(document.body.classList.contains("side-hidden")));

window.addEventListener(
  "keydown",
  (e) => {
    const handled = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (e.key === "F1") {
      handled();
      toggleHelp();
      return;
    }
    if (e.key === "Escape" && closeLightbox()) {
      handled();
      return;
    }
    if (e.key === "Escape" && !helpEl.hidden) {
      handled();
      toggleHelp(false);
      return;
    }
    if (e.key === "F2" && !e.ctrlKey && !e.shiftKey && !e.altKey) {
      handled();
      startRename();
      return;
    }
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    const k = e.key.toLowerCase();
    if (k === "p" && !e.shiftKey) {
      handled();
      if (state.root) palette.open(fileProvider);
    } else if (k === "o" && e.shiftKey) {
      handled();
      if (state.current) palette.open(headingProvider);
    } else if (k === "n" && !e.shiftKey) {
      handled();
      newFile();
    } else if (k === "l" && e.shiftKey) {
      handled();
      themeMenu.flip();
    } else if (k === "b" && !e.shiftKey) {
      handled();
      setSidebar(document.body.classList.contains("side-hidden"));
    } else if (k === "f" && e.shiftKey) {
      handled();
      showPanel("search");
      const { from, to } = editor.state.selection.main;
      search.focus(editor.state.sliceDoc(from, to));
    } else if (k === "e" && e.shiftKey) {
      handled();
      showPanel("files");
    } else if (k === "s" && !e.shiftKey) {
      handled();
      flushSave();
    } else if (k === "o" && !e.shiftKey) {
      handled();
      pickFolder();
    } else if (k === "1" || k === "2" || k === "3") {
      handled();
      setMode((["code", "split", "preview"] as const)[Number(k) - 1]);
    }
  },
  true,
);

listen<string[]>("files-changed", (e) => setFiles(e.payload));

// Edited outside the app (another editor, OneDrive/git sync): reload in place.
listen<string[]>("file-changed", async (e) => {
  const path = state.current;
  if (!path || !e.payload.includes(path)) return;
  let text: string;
  try {
    text = await invoke<string>("read_file", { path });
  } catch {
    return;
  }
  const norm = text.replace(/\r\n/g, "\n");
  if (path !== state.current || norm === state.disk) return; // our own save
  if (state.dirty) {
    toast(`${basename(path)} changed on disk while you were editing; keeping your version`);
    return;
  }
  state.disk = norm;
  state.eol = text.includes("\r\n") ? "\r\n" : "\n";
  replaceDoc(editor, norm);
  outline.update(norm);
  refreshPreview();
});

const win = getCurrentWindow();
win.onCloseRequested(async () => {
  await flushSave();
});

// ---------------------------------------------------------------- startup

applyMode();
if (store.get("sidebar") === "0") setSidebar(false);
(async () => {
  const arg = await invoke<string | null>("initial_path");
  const ws = arg ?? store.get("workspace");
  if (ws) await openWorkspace(ws, !arg);
})();
