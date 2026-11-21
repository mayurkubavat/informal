# CI & Batch Integration

This guide covers running InFormal headlessly in Makefiles, regression farms, and CI pipelines with deterministic exit codes, structured reports, proof caching, and memory guardrails.

---

## 1. Exit Codes for Automation

| Command | Exit code | Meaning |
| --- | --- | --- |
| `informal check <model>` | `20` | **SAFE** (unbounded proof) |
| `informal check <model>` | `10` | **UNSAFE** (counterexample found & replayed) |
| `informal check <model>` | `0` | **UNKNOWN** (timeout / inconclusive) |
| `informal shell --check <script>` | `0` / `1` | `0` = script clean; `1` = unsupported command/option or Tcl error |
| `informal shell <script>` | `0` / `1` | `0` = script ran to completion (check statuses in Tcl!); `1` = Tcl error |
| Any command | `3` | Hard memory budget exceeded (`--mem-gb` or cgroup limit) |

### Enforcing Pass/Fail in a Batch Tcl Script

Because `prove -all` completes normally even when an assertion returns `cex` (so you can inspect traces or run multiple tasks), add an explicit signoff assertion at the end of CI scripts:

```tcl
prove -all
report -csv -file "$env(OUT_DIR)/results.csv" -force

set failed [get_property_list -include {type assert status cex}]
set undet  [get_property_list -include {type assert status undetermined}]
set unreach [get_property_list -include {type cover status unreachable}]

if {[llength $failed] > 0 || [llength $undet] > 0} {
    error "FPV CI gate failed: cex={$failed} undetermined={$undet}"
}
```

---

## 2. Structured Outputs (CSV, JSON, HTML)

- **Property CSV:** `report -csv -file results.csv -force`
- **Per-property engine telemetry JSON:** `get_stats` (or `get_property_info <P> -json`)
- **Design metrics JSON:** `get_design_info -json`
- **Formal App reports:** `check_csr -report -json`, `check_protocol -report -json`, `check_sec -report -json`, `check_cov -report -html -report_file cov.html -no_return -force`
- **Script migration audit JSON:** `informal shell --check --json setup.tcl`

---

## 3. Sharing the Proof Cache Across CI Runs

Enable the SHA-256 content-addressed proof cache in a CI-cached directory (e.g., GitHub Actions `actions/cache` or NFS shared scratch):

```tcl
set_prove_cache on
set_prove_cache_path .informal_cache
set_prove_cache_mode read_write
```

Unchanged properties resolve immediately from cache; any change to RTL, assumptions, cut points, or the property expression automatically generates a new key.

---

## 4. Memory Caps & Process Isolation on Shared Farms

InFormal automatically inspects Linux cgroup v1/v2 `memory.max` / `memory.high` (defaulting to 90% of the cgroup cap, or 75% of physical RAM). For shared CI runners or LSF/Slurm farm slots:

```sh
informal --mem-gb 8 --isolate shell run.jg.tcl \
  --engines config/engines.toml \
  --workdir "$TMPDIR/informal_work" \
  --certificate "$TMPDIR/certs"
```

- `--isolate` (or `INFORMAL_ISOLATE=1`) spawns each in-house engine in its own OS process so a single engine OOM never takes down the session.
- `--workdir` isolates concurrent CI jobs so generated Btor2 and VCD files never collide.

---

## 5. Minimal Makefile & GitHub Actions Template

```makefile
INFORMAL ?= informal
WORKDIR  ?= build/informal_work

.PHONY: fpv-lint fpv-run fpv-cert
fpv-lint:
	$(INFORMAL) shell --check run.jg.tcl

fpv-run: fpv-lint
	$(INFORMAL) --isolate --mem-gb 8 shell run.jg.tcl --workdir $(WORKDIR)

fpv-cert: fpv-lint
	$(INFORMAL) --isolate shell run.jg.tcl --workdir $(WORKDIR) --certificate $(WORKDIR)/certs
	python3 -I scripts/certificate_check.py $(WORKDIR)/certs
```

See a complete runnable version in [examples/05-integration-and-signoff/signoff-spec/Makefile](../../examples/05-integration-and-signoff/signoff-spec/Makefile).

---

## Related Guides

- [Certificates & Signoff](../guide/certificates-and-signoff.md)
- [Engine & LLM Configuration](engines-config.md)
- [CLI Reference Manual](../reference/cli.md)
