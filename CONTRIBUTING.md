# Contributing to InFormal

Thanks for helping. Bug reports, benchmark results, new SVA or Tcl shell
coverage, and engine work are all welcome.

## Before you start

- For anything larger than a small fix, open an issue first so we can agree
  on the approach. `docs/decisions.md` records the design decisions; a
  change that goes against one should say so and why.
- Soundness comes first. An engine change must show 0 disagreements on the
  HWMCC'24 sample (`scripts/inhouse_sample.sh` then `scripts/hwmcc_check.py`)
  before it is merged.

## Making a change

1. Fork the repository and branch from `main`.
2. Build and test as described in the README. Run `scripts/check.sh`
   (formatting, clippy, and all Rust and UI tests) before you open a pull
   request; it must pass.
3. Keep commits focused, with messages that say what changed and why.
4. Open a pull request against `main` and describe how you tested it.

## Contributor License Agreement

InFormal is licensed under Apache-2.0 and owned by its author. Before your
first pull request can be merged, you need to agree to the Contributor
License Agreement in `CLA.md`. You keep the copyright in your contribution;
the agreement gives the project a license to use it, including the right
to distribute it under other licenses in future.

To agree, add this comment to your pull request, once:

> I have read the InFormal Contributor License Agreement (CLA.md) and I
> agree to it.

If you contribute as part of your job, make sure your employer allows it.
If they own your work, they need to agree to the CLA instead; open an issue
and we will sort that out.

## Third-party code

Do not paste code from other projects into a pull request. If a change
needs a new library or tool, say so in the issue first: its license must
allow an Apache-2.0 InFormal, and it goes in `docs/third-party.md` with
its license and pinned version. Do not copy text or screenshots from
commercial tool documentation.

## Licensing of contributions

Unless you say otherwise in writing, a contribution you submit is under the
CLA and therefore under Apache-2.0 as part of InFormal.
