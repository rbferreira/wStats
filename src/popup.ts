// The popup window: one webview, kept alive and hidden, that swaps modules when a strip
// item is clicked. Rust positions and shows the window once we report our height.

import "./styles/popup.css";
import { connect, inTauri, refresh, type State } from "./data";
import type { Module } from "./modules/common";
import { MODULES } from "./modules";
import { onLanguageChange } from "./i18n";
import { followSettings } from "./theme";
import type { ModuleId } from "./types";

const root = document.getElementById("app")!;
const built = new Map<ModuleId, Module>();
let current: Module | null = null;
let currentId: ModuleId | null = null;
let state: State | null = null;

async function reportSize() {
  if (!inTauri) return;
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("popup_size", { height: Math.ceil(root.getBoundingClientRect().height) });
}

function show(id: ModuleId) {
  let mod = built.get(id);
  if (!mod) {
    mod = MODULES[id]();
    built.set(id, mod);
  }
  current = mod;
  currentId = id;
  root.replaceChildren(mod.el);
  if (state) mod.update(state);
  mod.el.classList.remove("enter");
  void mod.el.offsetWidth; // restart the entrance animation
  mod.el.classList.add("enter");
  return reportSize();
}

// Content height changes when sections appear (processes arrive one tick after opening).
// Nothing to report before a module is in: an empty page's height would show an empty popup.
new ResizeObserver(() => {
  if (current) reportSize();
}).observe(root);

followSettings();

// Modules hold their text in the DOM: rebuild them in the new language.
onLanguageChange(() => {
  built.clear();
  if (currentId) show(currentId);
});

connect((s) => {
  state = s;
  current?.update(s);
});

if (inTauri) {
  import("@tauri-apps/api/event").then(({ listen }) => {
    listen<{ module: ModuleId; theme?: string }>("popup:show", async (e) => {
      if (e.payload.theme) document.documentElement.dataset.theme = e.payload.theme;
      // Nothing arrived while the window was hidden: catch up before drawing.
      await refresh().catch(console.error);
      show(e.payload.module);
    });
  });
  // Rebuilt after being unloaded to save memory: the click that did it is waiting.
  import("@tauri-apps/api/core").then(async ({ invoke }) => {
    const pending = await invoke<ModuleId | null>("popup_pending");
    if (pending) show(pending);
  });
} else {
  const params = new URLSearchParams(location.search);
  if (params.get("theme")) document.documentElement.dataset.theme = params.get("theme")!;
  show((params.get("m") as ModuleId) ?? "cpu");
}
