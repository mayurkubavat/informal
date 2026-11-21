# Track 04 — Formal Apps Mini-Labs

Seven self-contained designs exercising every built-in Formal App in InFormal. Each lab finishes in under 2 seconds and demonstrates both clean proofs and realistic bug/waiver workflows.

| App | Directory | What you will run |
| --- | --- | --- |
| **Coverage & UNR** | [`cov/`](cov/README.md) | `check_cov` — COI observability, branch & toggle UNR dead code, waivers |
| **Connectivity** | [`conn/`](conn/README.md) | `check_conn` — OpenTitan-style `CONNECTION`/`CONDITION`/`LATENCY` CSV checks |
| **X-Propagation** | [`xprop/`](xprop/README.md) | `check_xprop` — exact 2-copy X-prop catching reset-less leaks without mux pessimism |
| **Security Path (SPV)** | [`spv/`](spv/README.md) | `check_spv` — 2-safety key isolation, conditional `-cond`, multi-cycle covert leak |
| **Equivalence (SEC / LEC)** | [`sec/`](sec/README.md) | `check_sec` — sequential SEC, retimed `-latency 1`, combinational `-lec`, mismatch VCD |
| **CSR / RAL** | [`csr/`](csr/README.md) | `check_csr` — automated OpenTitan HJSON register verification (`RW`/`RO`/`RW1C`/`WO`/`regwen`) |
| **Bus Protocol VIP** | [`protocol/`](protocol/README.md) | `check_protocol` — 1-command formal APB4 slave VIP compliance proof |

See [docs/guide/formal-apps.md](../../docs/guide/formal-apps.md) for conceptual background and [docs/reference/tcl-commands.md](../../docs/reference/tcl-commands.md) for full option tables.
