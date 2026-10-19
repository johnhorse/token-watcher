// T6 —— adapter 选择：Tauri 运行时走真实 command/event，否则 mock。
// ?mock=1 可强制 mock（tauri dev 里做 UI 自查时用）。

import { createMockAdapter } from "./mock";
import { createTauriAdapter } from "./tauri";
import type { UiAdapter } from "./types";

export * from "./types";

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function createAdapter(): UiAdapter {
  const params = new URLSearchParams(window.location.search);
  if (params.get("mock") === "1") return createMockAdapter();
  return inTauri() ? createTauriAdapter() : createMockAdapter();
}
