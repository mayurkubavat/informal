# EDA GUI references for the InFormal workbench

Notes on how the established formal/simulation GUIs are laid out, and what
InFormal should take from them. Written from general familiarity with the
tools (JasperGold, VC Formal on Verdi, Xcelium/SimVision); the vendor user
guides for your release are the authority on exact menu and window names.
Public web material on these GUIs is thin.

## What the three tools have in common

- **The window frame is quiet.** Neutral grey or white panels, a classic menu
  bar, and a compact row of small icon buttons. Colour is kept for *data*:
  status icons, waveforms and source highlights. Buttons are not coloured.
- **Status is an icon first.** A small glyph (check, cross, circle, question
  mark, hourglass) carries the colour. The text next to it stays the normal
  text colour.
- **Tree on the left, table in the middle, console at the bottom.** The left
  tree is the way into the data (tasks, design scopes). The middle shows the
  list of properties or signals. The bottom holds the Tcl console with a
  plain prompt (`jg>`, `vcf>`, `xcelium>`).
- **Debug opens in its own view.** Counterexample/trace debug (Jasper
  *Visualize*, Verdi *nWave/nTrace*, SimVision *Waveform/Source Browser*) is a
  separate window or tab that you reach by double-clicking or "send to". It
  is not part of the main screen.
- **Cross-probing everywhere.** Selecting something in one view selects it in
  the others: property → source line, signal → waveform, scope → signal list.
- **Very dense.** Small UI font, about 20–24 px rows, almost no padding or
  rounded corners.

## JasperGold (Cadence)

- Main window: **Task Tree** on the left (tasks such as `<embedded>`, with
  their properties under them). The **property table** in the centre has
  status icon, name, type, engine, bound/trace length, time and task
  columns. The **Tcl console** (`jg>`) is at the bottom.
- An *application* picker (FPV, SEC, COV, CSR, …) decides which views show.
- Prove controls are a few small toolbar icons and context-menu items
  ("Prove", "Prove Task", "Visualize"). Most actions live in right-click
  menus on table rows.
- **Visualize** opens a CEX/cover trace in a waveform with the property's
  signals already added. **Proof Profiler** shows engine time.
- Status glyphs: proven, cex, covered, unreachable, undetermined, and
  in-progress (a spinning or hourglass icon while engines run). The summary
  is plain counts, not buttons.

## VC Formal (Synopsys, Verdi-based GUI)

- Runs inside the Verdi frame, so it uses Verdi's grey Qt look, dockable
  panes and a big menu bar.
- A **goal/property list** with status icons (proven, falsified,
  inconclusive, covered, uncoverable), with depth and time columns, grouped
  by app (FPV, FXP, SEQ, FCA, …). A **status summary** counts each status.
- Source in **nTrace** and waveforms in **nWave**. A failing goal opens its
  witness or counterexample straight in nWave.
- The `vcf>` shell console sits at the bottom. Its output is plain
  monospace, with only errors and warnings in colour.

## Xcelium / SimVision (Cadence)

- **Console** window with one tab per tool (a SimVision tab and a simulator
  tab), so Tcl for the GUI and Tcl for the simulator sit side by side.
- **Design Browser**: a scope tree on the left and the signals of the
  selected scope on the right, with live values. "Send to"
  waveform/source/schematic toolbar buttons.
- Separate **Waveform**, **Source Browser** and **Schematic Tracer** windows.
  Layouts can be saved and restored (SimVision writes them as Tcl).

## Why the current ui-redesign build feels busy

Compared with the tools above, there is too much colour and too many shapes
in the window frame itself:

1. A filled accent "Prove all" button, an accent send button, and accent tab
   underlines and rail highlights all compete for attention.
2. Status colour shows up three times (row text colour, dots, coloured count
   pills), and the pill row is a second filter bar.
3. Source preview highlights every line that mentions any signal, so most of
   the file turns yellow.
4. Too many styles: pills, chips, rounded tabs, underline tabs, a segmented
   control, a large rounded composer and a serif title.
5. Two top bars (doc tabs plus the filter/pill toolbar) before the data starts.

## Proposed "quiet workbench" revision

- **Menu bar + icon toolbar.** File / Edit / View / Prove / Tools / Window /
  Help, then one row of 24 px icon buttons with tooltips (Source script, Prove
  all, Prove selected, Stop, Visualize). No filled buttons. The accent shows
  only on focus, selection and the running-state indicator.
- **Neutral palette.** Keep the warm greys. Use accent teal only for
  selection and focus. Remove the serif title.
- **Status glyphs.** A 12 px coloured icon (✓ ✗ ◎ ⊘ ? ! and a spinner for
  running), with the label in the normal text colour. Summary counts become
  plain text in the status bar and at the top of the task tree, where
  clicking a status node in the tree filters the table (as the Jasper Task
  Tree does).
- **Task Tree on the left.** Task → status groups → properties, with counts.
  "Design" (scope tree + signal list, in the style of SimVision's Design
  Browser) is the second tab.
- **Property table.** 22 px rows. Columns: status icon, Name, Type, Engine,
  Bound, Time, Task. Right-click menu: Prove, Visualize, Show source, Copy
  name, Disable. Double-click opens Visualize (the trace tab) for CEX/cover,
  and source otherwise.
- **One tab style** everywhere (flat, underline), for document tabs and dock
  tabs alike.
- **Console in the style of JasperGold/VC Formal.** Plain scrollback and a
  one-line prompt input (`jg>` / `vcf>`), with no rounded composer. Tabs:
  Console | Messages (warnings/errors) | Log. Keep the Tcl/Ask toggle as a
  small control at the far right.
- **Source view.** Highlight only the property's line. Signal hits get a faint
  underline, not a line background.
- **Inspector.** Plain key/value list and expression, with no chips.
  Collapsible, and hidden by default when the window is narrow.
- **Later.** Save and restore the layout (as SimVision does), keyboard
  shortcuts (F5 prove, F6 prove selected, Esc stop), and a Proof Profiler tab.
