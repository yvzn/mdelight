import { isMarkdown } from "./util";

type Dir = { name: string; path: string; dirs: Map<string, Dir>; files: string[] };

const newDir = (name: string, path: string): Dir => ({ name, path, dirs: new Map(), files: [] });

/** Folder tree; only expanded folders are rendered, so huge workspaces stay cheap. */
export class FileTree {
  private root = newDir("", "");
  private expanded = new Set<string>();
  private active: string | null = null;

  constructor(
    private el: HTMLElement,
    private onOpen: (path: string) => void,
  ) {
    el.addEventListener("click", (e) => {
      const row = (e.target as HTMLElement).closest<HTMLElement>(".row");
      if (!row) return;
      const path = row.dataset.path!;
      if (row.classList.contains("dir")) {
        if (!this.expanded.delete(path)) this.expanded.add(path);
        this.render();
      } else this.onOpen(path);
    });
  }

  setFiles(files: string[]) {
    this.root = newDir("", "");
    for (const f of files) {
      const parts = f.split("/");
      let d = this.root;
      for (let i = 0; i < parts.length - 1; i++) {
        let child = d.dirs.get(parts[i]);
        if (!child) {
          child = newDir(parts[i], parts.slice(0, i + 1).join("/"));
          d.dirs.set(parts[i], child);
        }
        d = child;
      }
      d.files.push(f);
    }
    this.render();
  }

  setActive(path: string) {
    this.active = path;
    let changed = false;
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++) {
      const dir = parts.slice(0, i).join("/");
      if (!this.expanded.has(dir)) {
        this.expanded.add(dir);
        changed = true;
      }
    }
    if (changed) this.render();
    else {
      this.el.querySelector(".row.active")?.classList.remove("active");
      this.el.querySelector(`.row[data-path="${CSS.escape(path)}"]`)?.classList.add("active");
    }
    this.el.querySelector(".row.active")?.scrollIntoView({ block: "nearest" });
  }

  private render() {
    const frag = document.createDocumentFragment();
    this.renderDir(this.root, 0, frag);
    this.el.replaceChildren(frag);
  }

  private renderDir(d: Dir, depth: number, frag: DocumentFragment) {
    const dirs = [...d.dirs.values()].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
    for (const sub of dirs) {
      const open = this.expanded.has(sub.path);
      frag.append(this.row(sub.path, sub.name, depth, open ? "dir open" : "dir"));
      if (open) this.renderDir(sub, depth + 1, frag);
    }
    for (const f of d.files) {
      let cls = isMarkdown(f) ? "file md" : "file";
      if (f === this.active) cls += " active";
      frag.append(this.row(f, f.slice(f.lastIndexOf("/") + 1), depth, cls));
    }
  }

  private row(path: string, name: string, depth: number, cls: string): HTMLElement {
    const row = document.createElement("div");
    row.className = "row " + cls;
    row.dataset.path = path;
    row.style.paddingLeft = `${6 + depth * 14}px`;
    row.title = path;
    const tw = document.createElement("span");
    tw.className = "tw";
    const label = document.createElement("span");
    label.className = "label";
    label.textContent = name;
    row.append(tw, label);
    return row;
  }
}
