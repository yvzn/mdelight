import { isDark, onThemeChange, setThemePref, themePref, type ThemePref } from "./theme";
import { $ } from "./util";

const svg = (body: string) =>
  `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" aria-hidden="true">${body}</svg>`;

const ICONS: Record<ThemePref, string> = {
  light: svg(
    `<circle cx="8" cy="8" r="3"/><path d="M8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1"/>`,
  ),
  dark: svg(`<path d="M13.5 9.6A5.5 5.5 0 0 1 6.4 2.5a5.5 5.5 0 1 0 7.1 7.1z"/>`),
  system: svg(`<circle cx="8" cy="8" r="5.5"/><path d="M8 2.5a5.5 5.5 0 0 1 0 11z" fill="currentColor"/>`),
};

const LABELS: Record<ThemePref, string> = { light: "Light", dark: "Dark", system: "Match Windows" };
const ORDER: ThemePref[] = ["light", "dark", "system"];

/** Toolbar theme switch: icon shows the current choice, click opens a 3-option menu. */
export function setupThemeMenu() {
  const btn = $("#theme-btn");
  const menu = $("#theme-menu");

  function render() {
    const pref = themePref();
    btn.innerHTML = ICONS[pref];
    const now = pref === "system" ? ` (now ${isDark() ? "dark" : "light"})` : "";
    btn.title = `Theme: ${LABELS[pref]}${now} · Ctrl+Shift+L`;
    menu.innerHTML = ORDER.map(
      (p) =>
        `<button role="menuitemradio" aria-checked="${p === pref}" data-pref="${p}">${ICONS[p]}<span>${LABELS[p]}</span><span class="check">✓</span></button>`,
    ).join("");
  }

  function toggle(open = menu.hidden) {
    menu.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
    if (open) menu.querySelector<HTMLElement>('[aria-checked="true"]')?.focus();
  }

  btn.addEventListener("click", () => toggle());
  menu.addEventListener("click", (e) => {
    const item = (e.target as HTMLElement).closest<HTMLElement>("[data-pref]");
    if (!item) return;
    setThemePref(item.dataset.pref as ThemePref);
    render();
    toggle(false);
    btn.focus();
  });
  menu.addEventListener("keydown", (e) => {
    const items = [...menu.querySelectorAll<HTMLElement>("button")];
    const i = items.indexOf(document.activeElement as HTMLElement);
    if (e.key === "Escape") {
      toggle(false);
      btn.focus();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
    }
  });
  document.addEventListener("mousedown", (e) => {
    if (!menu.hidden && !(e.target as HTMLElement).closest("#theme-menu, #theme-btn")) toggle(false);
  });
  onThemeChange(render);
  render();

  return {
    /** Ctrl+Shift+L: flip to the opposite of what's on screen. */
    flip() {
      setThemePref(isDark() ? "light" : "dark");
      render();
    },
  };
}
