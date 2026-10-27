import { EditorView, basicSetup } from "codemirror";
import { Annotation, EditorState, Compartment } from "@codemirror/state";
import { LanguageDescription } from "@codemirror/language";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { languages } from "@codemirror/language-data";
import { oneDark } from "@codemirror/theme-one-dark";
import { isDark, onThemeChange } from "./theme";
import { basename, isMarkdown } from "./util";

const lang = new Compartment();
const theme = new Compartment();
/** Marks edits that came from disk rather than the user. */
const external = Annotation.define<boolean>();
let onChange: () => void = () => {};

function makeState(doc: string): EditorState {
  return EditorState.create({
    doc,
    extensions: [
      basicSetup,
      EditorView.lineWrapping,
      lang.of([]),
      theme.of(isDark() ? oneDark : []),
      EditorView.updateListener.of((u) => {
        if (u.docChanged && !u.transactions.some((t) => t.annotation(external))) onChange();
      }),
    ],
  });
}

export function createEditor(parent: HTMLElement, hooks: { onChange: () => void; onScroll: () => void }): EditorView {
  onChange = hooks.onChange;
  const view = new EditorView({ parent, state: makeState("") });
  view.scrollDOM.addEventListener("scroll", hooks.onScroll, { passive: true });
  onThemeChange((dark) => view.dispatch({ effects: theme.reconfigure(dark ? oneDark : []) }));
  return view;
}

/** Replace the document (fresh undo history); language support loads lazily. */
export function setDocument(view: EditorView, doc: string, path: string) {
  const state = makeState(doc);
  view.setState(state);
  const load = isMarkdown(path)
    ? Promise.resolve(markdown({ base: markdownLanguage, codeLanguages: languages }))
    : LanguageDescription.matchFilename(languages, basename(path))?.load();
  load?.then((ext) => {
    // Another file may have been opened while the language loaded.
    if (view.state.doc === state.doc) view.dispatch({ effects: lang.reconfigure(ext) });
  });
}

/** Swap in new text by replacing only the differing middle, so cursor and scroll stay put. */
export function replaceDoc(view: EditorView, text: string) {
  const cur = view.state.doc.toString();
  const max = Math.min(cur.length, text.length);
  let s = 0;
  while (s < max && cur.charCodeAt(s) === text.charCodeAt(s)) s++;
  let e = 0;
  while (e < max - s && cur.charCodeAt(cur.length - 1 - e) === text.charCodeAt(text.length - 1 - e)) e++;
  view.dispatch({
    changes: { from: s, to: cur.length - e, insert: text.slice(s, text.length - e) },
    annotations: external.of(true),
  });
}

/** Fractional line number at the top of the editor viewport. */
export function topLine(view: EditorView): number {
  const top = view.scrollDOM.scrollTop;
  const block = view.lineBlockAtHeight(top);
  const line = view.state.doc.lineAt(block.from).number;
  return line + Math.max(0, Math.min(1, (top - block.top) / Math.max(1, block.height)));
}

export function scrollToLine(view: EditorView, line: number) {
  const n = Math.max(1, Math.min(view.state.doc.lines, Math.floor(line)));
  const block = view.lineBlockAt(view.state.doc.line(n).from);
  view.scrollDOM.scrollTop = block.top + (line - n) * block.height;
}

export function gotoLine(view: EditorView, line: number, y: "center" | "start" = "center") {
  const l = view.state.doc.line(Math.max(1, Math.min(view.state.doc.lines, line)));
  view.dispatch({ selection: { anchor: l.from }, effects: EditorView.scrollIntoView(l.from, { y, yMargin: 8 }) });
  view.focus();
}
