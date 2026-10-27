import { store } from "./util";

/** "system" follows Windows; the others force a theme. */
export type ThemePref = "system" | "light" | "dark";

const media = matchMedia("(prefers-color-scheme: dark)");
const listeners = new Set<(dark: boolean) => void>();
let pref: ThemePref = readPref();
let dark = resolve();

function readPref(): ThemePref {
  const v = store.get("theme");
  return v === "light" || v === "dark" ? v : "system";
}

function resolve(): boolean {
  return pref === "dark" || (pref === "system" && media.matches);
}

function apply() {
  // CSS colors are light-dark() pairs; color-scheme (set via data-theme) picks the side.
  if (pref === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = pref;
  const next = resolve();
  if (next !== dark) {
    dark = next;
    for (const fn of listeners) fn(dark);
  }
}

media.addEventListener("change", apply);
apply();

export const isDark = () => dark;
export const themePref = () => pref;

export function setThemePref(p: ThemePref) {
  pref = p;
  store.set("theme", p);
  apply();
}

/** Called with the new value whenever the effective theme flips. */
export function onThemeChange(fn: (dark: boolean) => void) {
  listeners.add(fn);
}
