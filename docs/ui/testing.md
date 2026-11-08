# UI tests

All suites run against the browser mock (`ui/src/mock.ts`), so no Rust or
Tauri build is needed. Run from `ui/`.

| Command | What | Where |
|---|---|---|
| `npm test` | Vitest unit + component tests (jsdom) | `src/**/*.test.ts(x)` |
| `npm run test:e2e` | Playwright workflows + axe (serious/critical) | `tests/e2e/` |
| `npm run test:visual` | Screenshot diffs, light/dark x 1440x900/1000x700 | `tests/visual/` |
| `npm run test:update` | Regenerate screenshot baselines | `tests/__screenshots__/` |

Playwright starts `npm run dev` on :1420 (or reuses a running one). First run
on a new machine: `npx playwright install chromium`. Baselines carry the
platform in their file names (`-linux`, `-darwin`), so each OS keeps its own;
generate them with `test:update` and review the PNGs before committing.

Rules: wait with `await expect(...)` polling, never fixed sleeps; mask
anything time-based (Messages time and duration columns).
