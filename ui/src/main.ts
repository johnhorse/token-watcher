import { mount } from "svelte";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import App from "./App.svelte";
import HudApp from "./HudApp.svelte";
import "./theme.css";

let isHud = false;
if (typeof window !== "undefined") {
  if (window.location.search.includes("window=hud") || window.location.hash === "#hud") {
    isHud = true;
  } else if ("__TAURI_INTERNALS__" in window) {
    try {
      isHud = getCurrentWebviewWindow().label === "hud";
    } catch {
      isHud = false;
    }
  }
}

const app = mount(isHud ? HudApp : App, {
  target: document.getElementById("app")!,
});

export default app;
