# UI tests

All suites run against the browser mock (`ui/src/mock.ts`), so no Rust or
Tauri build is needed. Run from `ui/`.

| Command | What | Where |
|---|---|---|
| `npm test` | Vitest unit + component tests (jsdom) | `src/**/*.test.ts(x)` |
| `npm run test:e2e` | Playwright workflows + axe (serious/critical) | `tests/e2e/` |
| `npm run test:visual` | Screenshot diffs, light/dark x 1440x900/1000x700 | `tests/visual/` |
| `npm run test:update` | Regenerate screenshot baselines | `tests/__screenshots__/` |

Playwright starts its own dev server on :1431 (`UI_TEST_PORT` changes it) and
never reuses a running one, so it always tests this checkout. First run
on a new machine: `npx playwright install chromium`. Baselines carry the
platform in their file names (`-linux`, `-darwin`), so each OS keeps its own;
generate them with `test:update` and review the PNGs before committing.

Rules: wait with `await expect(...)` polling, never fixed sleeps; mask
anything time-based (Messages time and duration columns).

The mock proves asynchronously (see `docs/ui/stop-progress.md`). Query
parameters steer it: `?fast=1` finishes a prove in one step (most tests use
it), `?tick=<ms>` sets the tick, `?freeze=<n>` holds a run after n ticks (a
deterministic mid-run frame for the running/stopping baselines and Stop
tests), `?holdStop=1` keeps "Stopping…" up, `?props=<n>` adds n generated
properties (p_00000 ...) for the virtualized-table tests (5,000 in e2e). Sourcing `mock/prove-on-source.tcl`
adds two properties and proves them in the same call (the session only gets
them with the reply), for the provisional-row path. Unit tests set `mockOptions`
and call `resetMock()` instead.

AI interface: `?ai=1` starts the mock as if an agent had proposed an
assumption (`m_en_hold`) and a helper assertion (`a_q_bound`), both pending;
`mockAiPropose` does the same from a unit test. `tests/e2e/proposals.spec.ts`
and the `proposals` baselines cover the Proposals tab.

Traces: after a prove, the mock gives `a_never9` (cex) and `c_done` (covered)
`trace` paths (`informal_work/<prop>.vcd`), and `readTrace` serves the hand-written
counter VCD `ui/tests/fixtures/a_never9.vcd` for both (12 cycles, `en` low in
cycle 3, `q` reaches 9 and `done` rises in cycle 11). `src/vcd.test.ts` parses
it; `TraceView.test.tsx`, the e2e "trace tab" block and the `trace*` baselines
drive the trace tab from it.
