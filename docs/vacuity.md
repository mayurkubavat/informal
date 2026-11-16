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
whatever the design does, so its proof says nothing beyond that step;
these conflict, and dropping any one of them allows a longer trace:
conflict.m_on, conflict.m_stop
```

The set is found by dropping each assumption in turn (one more bounded
run each) and leaving it out while the rest still allow no longer trace:
an assumption that takes no part in the conflict is not listed, and
dropping any one that is listed allows a longer trace. When several
different sets conflict, this finds one of them. `reset` is listed when
the reset itself is part of the conflict. svlang names each `constraint`
line after its assumption, which is what makes this possible.

and every property proven in that run carries the vacuity
`vacuous (environment)`: in the report's detail column
(`[vacuous (environment)]`), in `informal::session` (`"vacuity":
"vacuous-environment"`) and so in the UI and the MCP replies. The status
word stays `proven`, so scripts that filter on it work as before.

`check_assumptions [-depth N]` (`informal::check_assumptions` in every
flavor) runs the same check on its own and returns the number of steps the
environment allows, up to N; with `-conflict` it returns the conflicting
assumptions instead, as a list (empty when they do not conflict).

Steps are model steps: one per clock cycle for a design on one rising edge,
two per cycle when the model drives the clocks (both edges, several clocks).
The reset's own steps count.

**Precondition covers** run after every prove that found no conflict.
For each proven assertion with an antecedent (an implication, a procedural
enable such as an `if` around it), svlang compiles a cover of that
antecedent from the same property: same clock, same reset, same
`disable iff`, same procedural enable and the same first ticks. It is
named `<assertion>:precondition` in the model and is not listed as a
property of its own. Its result is the assertion's vacuity:

| Cover | Vacuity | Meaning |
|---|---|---|
| hit, trace replays | `non-vacuous` | some attempt of the assertion was checked |
| hit, model has stopats or black boxes | `non-vacuous (cut points)` | the same, but the trace may use values the real logic cannot produce |
| unreachable | `vacuous` | no attempt is ever checked: the proof says nothing |
| neither within the time limit | `vacuity not checked` | |

A hit is checked like any cover, by replaying its trace on the model; an
unreachable cover gets a certificate like a proof when certificates are
on. Assertions without an antecedent (`a |-> b` has one, `cnt <= 5` has
none) and those with `until` get no cover and no vacuity. The prove log
names the vacuous ones:

```
informal: VACUOUS: 1 proven assertion(s) never check anything, because no
trace reaches their antecedent (precondition unreachable): request.a_off
```

`elaborate -no_preconditions` leaves the covers out from then on, and
`elaborate -create_related_covers {precondition}` asks for them again (as
in JasperGold). `-create_related_covers {witness}` is accepted with a
note: witness covers (antecedent and consequent together) are compiled by
svlang but not run yet. `informal::set_vacuity off` switches off both the
environment check and the precondition covers.

## Limits

- Twenty steps is a bound: assumptions that conflict only later are not
  found by this check. The precondition covers find their effect on each
  property.
- A precondition cover says whether some attempt is checked, not whether
  the consequent ever matters: an assertion whose consequent always holds
  is non-vacuous by this check. Witness covers and the design-free checks
  (planned) look at that.
- The conflicting set is one set, smallest in the sense that no member can
  be dropped, not the smallest of all; with more than 64 assumptions it is
  not narrowed. Script assumptions are not enforced while the reset is
  held, so one that contradicts the reset value is no conflict.
- When the in-house BMC is not available (no CaDiCaL) or does not finish
  within the time limit (at most 60 s a probe), the log says the check
  could not decide, and no vacuity is set.

## Planned, in this order

1. Witness covers run on request (`-create_related_covers {witness}`),
   and VC Formal's `check_fv -subtype` honoured.
2. Checks of each property on its own, without the design: an
   antecedent that cannot match, a property that always holds.
3. `get_needed_assumptions -property P`: the assumptions a proof or an
   unreachable cover uses, from solver cores.
4. `check_assumptions -dead_end`: reachable states from which no input
   meets the assumptions.
