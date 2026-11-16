# Vacuity and over-constraint checks

A proof is vacuous when it holds for a reason that has nothing to do with
what the property was meant to check: an antecedent that never fires, or
assumptions that allow no trace at all. InFormal reports `proven` for such
a proof like any other, and says beside it what the vacuity checks found
(decision D19). This page describes what is checked today and what is
planned; the research and design behind it are in the project's
"Vacuity and over-constraint checks: research and design" document.

## What is checked

**The environment check** runs before every prove (on by default;
`informal::set_vacuity off` switches it off). It asks whether the
assumptions and the reset allow a trace of 20 model steps. A trace of n
steps that meets every constraint has prefixes of every shorter length that
do too, so when the answer is no, bisection finds the longest trace they
allow, each probe one bounded run of informal-bmc on the model with its
targets taken out and a step counter put in.

When they allow fewer steps than asked, the prove log says so:

```
informal: VACUOUS ENVIRONMENT: the reset and m_on, m_stop allow no trace
longer than 2 step(s) (asked for 20). Every assertion holds from step 3 on
whatever the design does, so its proof says nothing beyond that step; ...
```

and every property proven in that run carries the vacuity
`vacuous (environment)`: in the report's detail column
(`[vacuous (environment)]`), in `informal::session` (`"vacuity":
"vacuous-environment"`) and so in the UI and the MCP replies. The status
word stays `proven`, so scripts that filter on it work as before.

`check_assumptions [-depth N]` (`informal::check_assumptions` in every
flavor) runs the same check on its own and returns the number of steps the
environment allows, up to N.

Steps are model steps: one per clock cycle for a design on one rising edge,
two per cycle when the model drives the clocks (both edges, several clocks).
The reset's own steps count.

## Limits

- Twenty steps is a bound: assumptions that conflict only later are not
  found by this check. The precondition covers below find their effect on
  each property.
- The conflict is not yet narrowed to the assumptions that cause it: the
  log lists every assumption in force. Narrowing needs the front end to
  name each `constraint` line after its assumption.
- When the in-house BMC is not available (no CaDiCaL) or does not finish
  within the time limit (at most 60 s a probe), the log says the check
  could not decide, and no vacuity is set.

## Planned, in this order

1. Precondition covers: for every proven assertion with an antecedent, a
   cover of the antecedent compiled by svlang from the same property (same
   clock, reset, `disable iff`, procedural enable and first ticks).
   Unreachable means `vacuous`; hit means `non-vacuous`, or `non-vacuous
   (cut points)` with stopats or black boxes. Witness covers on request;
   `elaborate -create_related_covers`, `-no_preconditions` and VC Formal's
   `check_fv -subtype` honoured.
2. Checks of each property on its own, without the design: an
   antecedent that cannot match, a property that always holds.
3. `get_needed_assumptions -property P`: the assumptions a proof or an
   unreachable cover uses, from solver cores.
4. `check_assumptions -dead_end`: reachable states from which no input
   meets the assumptions.
