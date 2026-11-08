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

The mock proves asynchronously (see `docs/ui/stop-progress.md`). Query
parameters steer it: `?fast=1` finishes a prove in one step (most tests use
it), `?tick=<ms>` sets the tick, `?freeze=<n>` holds a run after n ticks (a
deterministic mid-run frame for the running/stopping baselines and Stop
tests), `?holdStop=1` keeps "Stopping…" up. Sourcing `mock/prove-on-source.tcl`
adds two properties and proves them in the same call (the session only gets
them with the reply), for the provisional-row path. Unit tests set `mockOptions`
and call `resetMock()` instead.
