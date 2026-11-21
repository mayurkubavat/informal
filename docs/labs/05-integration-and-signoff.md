# Lab 05 — CI Gating, Certificates & `fvp.spec.json` Signoff

**Level:** Production / CI · **Directory:** `examples/05-integration-and-signoff/signoff-spec/`  
**Concepts:** `informal shell --check`, CI exit-code gating, independent `--certificate` verification, `Makefile` integration, and `fvp.spec.json` (`informal.spec/1`) audited by `spec_audit`.

## Run via CLI or Make

```sh
# 1. Pre-flight script check (no elaboration or solvers)
informal shell --check examples/05-integration-and-signoff/signoff-spec/run.jg.tcl

# 2. Full CI signoff run (fails non-zero if any assertion fails or cover is unreachable)
informal shell examples/05-integration-and-signoff/signoff-spec/run.jg.tcl

# 3. Export independent Certifaiger / aigsim certificates
informal shell examples/05-integration-and-signoff/signoff-spec/run.jg.tcl \
  --certificate informal_work/lab05_certs
python3 -I scripts/certificate_check.py informal_work/lab05_certs
```

Or via the included `Makefile`:

```sh
make -C examples/05-integration-and-signoff/signoff-spec fpv-run
```

## Auditing Against `fvp.spec.json` (MCP / AI)

Open `examples/05-integration-and-signoff/signoff-spec/run.jg.tcl` in an `informal mcp` session (or `InFormal.app` + `informal mcp --connect`), run `prove`, and invoke `spec_audit`:

```json
{
  "tool": "spec_audit",
  "arguments": {
    "spec_path": "examples/05-integration-and-signoff/signoff-spec/fvp.spec.json",
    "check_dead_end": true
  }
}
```

All three requirements (`REQ-SCNT-001`..`003`) report `verified` and `signoff_ready: true`. See [Certificates & Signoff](../guide/certificates-and-signoff.md) and [Formal Spec Schema](../reference/spec-schema.md).
