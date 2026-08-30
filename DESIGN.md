---
name: "Coop"
description: "A precise run-to-proof operations desk for monitoring execution and exporting server-reported evidence."
colors:
  workbench: "#ffffff"
  canvas: "#f7f9fc"
  quiet-plane: "#fbfcfe"
  border: "#dfe5ed"
  border-strong: "#b8c2d0"
  text: "#111827"
  text-muted: "#526077"
  text-faint: "#68758a"
  kernel: "#2458e6"
  focus: "#1d4ed8"
  healthy: "#18794e"
  warning: "#8a5700"
  danger: "#c42d35"
  cancelled: "#64748b"
  selection: "#eef4ff"
  code-bg: "#0d1117"
  code-text: "#d7dee8"
typography:
  headline:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "20px"
    fontWeight: 700
    lineHeight: 1.45
    letterSpacing: "-0.02em"
  title:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "15px"
    fontWeight: 700
    lineHeight: 1.45
    letterSpacing: "-0.015em"
  body:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.45
    letterSpacing: "normal"
  small-body:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "11px"
    fontWeight: 400
    lineHeight: 1.45
    letterSpacing: "normal"
  label:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "10px"
    fontWeight: 700
    lineHeight: 1.3
    letterSpacing: "0.04em"
  mono:
    fontFamily: "SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: "22px"
    letterSpacing: "normal"
  run-identifier:
    fontFamily: "SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace"
    fontSize: "18px"
    fontWeight: 650
    lineHeight: 1.3
    letterSpacing: "-0.02em"
rounded:
  none: "0"
  sm: "4px"
  control: "5px"
  overlay: "6px"
  circle: "50%"
components:
  button-primary:
    backgroundColor: "{colors.kernel}"
    textColor: "{colors.workbench}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "38px"
  button-secondary:
    backgroundColor: "{colors.workbench}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "38px"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.text-muted}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "38px"
  input:
    backgroundColor: "{colors.workbench}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 10px"
    height: "40px"
  context-chip:
    backgroundColor: "{colors.workbench}"
    textColor: "{colors.text-muted}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "6px 9px"
    height: "36px"
  tab-active:
    backgroundColor: "transparent"
    textColor: "{colors.kernel}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    padding: "8px 11px"
    height: "40px"
  selected-run-row:
    backgroundColor: "{colors.selection}"
    textColor: "{colors.text}"
    typography: "{typography.small-body}"
    rounded: "{rounded.none}"
    padding: "8px 12px"
    height: "58px"
  output-viewport:
    backgroundColor: "{colors.code-bg}"
    textColor: "{colors.code-text}"
    typography: "{typography.mono}"
    rounded: "{rounded.control}"
  evidence-section:
    backgroundColor: "{colors.workbench}"
    textColor: "{colors.text}"
    typography: "{typography.small-body}"
    rounded: "{rounded.none}"
    padding: "15px 16px"
---

# Design System: Coop

## Overview

**Creative North Star: "The Run-to-Proof Operations Desk"**

Coop is a precise, evidence-first control plane: cool-white working planes, ink and navy text, restrained indigo interaction, and fine structural rules. It should feel like a current operations product built for consequential work, never like a terminal-themed demo or a dashboard made from equal-weight cards.

The visual story follows the operator's job in one direction: connect a credential, choose or create a run, follow ordered execution, compare requested policy with observed posture, then export exact server-provided proof. Dense information stays legible because the interface uses rows, dividers, compact type, explicit labels, and honest state qualifiers instead of decoration.

The dashboard's three-pane desk is the flagship composition for this workflow: queue on the left, output-first run workspace in the center, and boundary/evidence inspector on the right. It is task-local, not a requirement that every Coop surface use three panes. New surfaces should inherit the same hierarchy, materials, and evidence discipline while choosing the composition their task needs.

**Key Characteristics:**

- Cool-white application planes separated by true one-pixel dividers.
- Compact ink-first typography with monospace reserved for machine-authored facts.
- One saturated indigo voice for action, selection, focus, and active navigation.
- Semantic green, amber, red, and slate paired with explicit labels and shapes.
- Dark material confined to code, raw output, and exact JSON evidence.
- Responsive movement from simultaneous panes to focused queue, detail, and record views.

## Colors

The palette is cool, quiet, and operational: neutral planes carry most of the screen while indigo and semantic hues remain scarce enough to communicate state.

### Primary

- **Kernel Indigo** (`kernel`): Primary actions, active tabs, selected-row rails, links, completed execution stages, checkboxes, and other deliberate interaction states.
- **Focus Indigo** (`focus`): The visible keyboard-focus outline and the stronger indigo endpoint; it is functional, not an extra decorative accent.

### Secondary

- **Evidence Green** (`healthy`): Connected, succeeded, and terminal-success states only.
- **Caution Amber** (`warning`): Active work, reconnecting, credential guidance, integrity cautions, and other states that require attention without implying failure.
- **Boundary Red** (`danger`): Failure, unsafe or absent runtime posture, destructive actions, and blocking errors.
- **Cancelled Slate** (`cancelled`): Neutral terminal outcomes that should not read as either success or failure.

### Neutral

- **Workbench White** (`workbench`): The dominant application and control surface.
- **Cool Canvas** (`canvas`): Very quiet hover, blank-glyph, and secondary plane treatment.
- **Quiet Plane** (`quiet-plane`): Section headers, filters, dialog chrome, and ledger footers that need separation without elevation.
- **Hairline Divider** (`border`): Structural seams between panes, rows, telemetry cells, tabs, and evidence sections.
- **Control Border** (`border-strong`): Inputs, buttons, overlays, and code-container edges that require firmer definition.
- **Operational Ink** (`text`): Headings, identifiers, strong values, and primary copy.
- **Muted Navy** (`text-muted`): Metadata, secondary copy, control labels, and inactive navigation.
- **Faint Slate** (`text-faint`): Column headings, tertiary metadata, and low-priority supporting information.
- **Selected Wash** (`selection`): The current queue row behind its indigo rail.
- **Code Night** (`code-bg`): Raw output, event ledgers, source input, and exact JSON only.
- **Code Frost** (`code-text`): Default text on Code Night.

**The Indigo Means Intent Rule.** Use indigo for primary action, current selection, active navigation, focus, and execution progression; never spread it across decorative surfaces.

**The Semantic State Rule.** Green, amber, red, and slate must report a real state and remain paired with text, a marker, or both; color alone never carries status.

**The Dark Containment Rule.** Dark material belongs only to code, output, events, and exact evidence payloads. The application shell stays light.

## Typography

**Display Font:** System UI sans (with ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif)

**Body Font:** System UI sans (with ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif)

**Label/Mono Font:** System monospace (with SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace)

**Character:** The system is compact and unshowy. Hierarchy comes from weight, spacing, alignment, and selective monospace rather than oversized display typography.

### Hierarchy

- **Headline** (700, 20px, 1.45): Product identity and top-level empty-state headings.
- **Run Identifier** (650, 18px, 1.3): The selected job ID; monospace makes the machine identifier visually authoritative.
- **Title** (700, 15px, 1.45): Pane titles and primary section headings.
- **Body** (400, 13px, 1.45): Controls, descriptive copy, and general operational content.
- **Small Body** (400, 11px, 1.45): Metadata, inspector explanations, toolbar summaries, and compact evidence labels.
- **Label** (700, 10px, 0.04em letter spacing, uppercase when status or stage): Execution stages, status badges, and dense table labels.
- **Mono** (400, 12px, 22px line height): Output and event rows; compact metadata uses the same family at 10–11.5px where the implementation establishes it.

**The Data Is Monospace Rule.** Use monospace for IDs, code, JSON, event channels, timestamps, hashes, and exact values; keep product prose and controls in the system sans.

**The Quiet Hierarchy Rule.** Do not introduce marketing-scale type. Operational hierarchy should be visible at a scan through weight, rhythm, and alignment.

## Layout

The desktop dashboard uses a sticky command bar (72px) above a viewport-filling three-pane grid: a 300px run queue, a flexible workspace with a 430px minimum, and a 300px proof inspector. At 1240px the side panes tighten to 286px. Panes meet on one-pixel rules with no gutters, making the workbench feel continuous while keeping ownership clear.

Spacing is dense and repeatable: 4–8px for tight inline relationships, 10–16px for controls and section padding, 18–20px for larger separations, and 32px for centered blank-state breathing room. Use blank space inside the relevant plane; do not manufacture rhythm with detached card grids.

At 1060px and below, simultaneous panes become focused screens. The queue occupies the viewport until a run is selected, then a Back to queue action leads a single detail view; the desktop inspector becomes a Record tab after Output, Events, and Result. At 720px, controls gain a 44px minimum target, the run hero stacks, filters remain a usable two-column group, and result metrics collapse to two columns. At 440px, header actions, toolbars, queue columns, tabs, and dialogs compact again while preserving a 320px minimum document width and horizontal scrolling for exact technical content.

**The Composition Stays Local Rule.** Preserve this queue → workspace → proof anatomy for the run ledger, but do not copy its three columns onto unrelated Coop surfaces.

**The Evidence Order Rule.** Responsive reflow may change simultaneity, never sequence: identity and state precede output; requested policy precedes observed posture; portable proof follows both.

## Elevation & Depth

The system is flat by default. Depth comes from tonal changes, one-pixel dividers, and bounded scroll regions; ordinary panes, rows, telemetry, tabs, fields, and evidence sections do not float. Shadows are reserved for elements that physically overlay the workbench.

### Shadow Vocabulary

- **Context Disclosure** (`0 16px 38px rgba(31, 42, 61, .16)`): Authenticated execution context opened from the connection chip.
- **Dialog Lift** (`0 24px 70px rgba(24, 36, 54, .22)`): The create-run modal above its dimmed backdrop.
- **Toast Lift** (`0 12px 28px rgba(31, 42, 61, .18)`): Temporary feedback at the viewport edge.
- **Selection Rail** (`inset 3px 0 0 var(--kernel)`): Structural current-row emphasis; this is selection, not surface elevation.

**The Flat-by-Default Rule.** If an element does not overlap the workbench, separate it with plane, spacing, or divider before considering shadow.

## Shapes

Coop uses compact, controlled geometry: square panes and rows, gently rounded controls (5px), smaller micro-elements (4px), and slightly softer overlays (6px). Status dots and execution markers are true circles. Borders are thin and cool; clipping appears where a bounded code surface or overlay needs it.

**The Rows Are Rows Rule.** Queue items, metrics, telemetry, and evidence sections meet on shared dividers; do not convert them into individually rounded cards.

**The Small-Radius Rule.** Keep controls technical and precise. Large bubbles, pill containers, and gratuitous rounding do not belong in this system.

## Components

### Buttons

- **Shape:** Compact rectangular control with a 5px radius, 38px minimum height, and 8px × 12px padding; small actions use a 34px minimum height and 6px × 9px padding.
- **Primary:** Kernel Indigo with white text and matching border. It owns the main action such as New run, Create a run, or Queue run.
- **Secondary:** Workbench White with Operational Ink and a Control Border. It carries concrete utility actions such as Apply or Copy ID.
- **Quiet:** Transparent with Muted Navy; hover introduces a quiet plane and divider border. Use for dismiss, refresh, clear, and copy actions that must not compete with the primary path.
- **Danger:** White with a restrained red border and text; hover adds a pale red plane. Destructive actions use an explicit confirmation step.
- **Hover / Focus / Disabled:** Hover darkens or strengthens the relevant edge; keyboard focus always uses the global 3px Focus Indigo outline with a 2px offset; disabled controls remain present at 55% opacity.

### Context Chips

- **Style:** 36px-high white controls with a 5px radius, 6px × 9px padding, a fine border, and a dot-plus-label pattern.
- **State:** Neutral gray means unknown, green means ready, amber means caution, and red means error or weak boundary. Warning and danger variants add a pale tonal plane and matching border; the explicit value remains visible.

### Inputs / Fields

- **Style:** White field, Control Border, 5px radius, and 8px × 10px padding. Search reserves inline space for a 14px icon; code fields switch to Code Night and monospace.
- **Hover / Focus:** Hover strengthens the border; visible focus uses the global outline without relying on border color alone.
- **Help / Error:** Help text is faint and compact. Errors use the danger role and remain textual.

### Run Queue Rows

- **Structure:** True table-like rows with status, identity, and duration columns; each row is at least 58px high and ends on a divider.
- **Selected:** Selected Wash plus an inset 3px Kernel Indigo rail. Selection uses both fill and rail so it survives scanning and high-density data.
- **Status:** A dot or spinner plus an uppercase label. Running rotates a partially open ring; terminal states fill the marker. Reduced-motion preferences remove rotation without removing the label.

### Execution Spine

- **Structure:** Three equal stages—Queued, Running, Terminal—connected by one-pixel lines, with monospaced timestamps beneath compact uppercase labels.
- **State:** Indigo shows completed progression, amber shows active work, green shows successful termination, red shows failed termination, and slate shows cancelled termination. Every state remains named in text.

### Evidence Tabs and Inspector

- **Navigation:** Tabs sit directly on a divider. The selected tab uses indigo text and a 2px underline; optional counts use a compact pale-indigo badge. On desktop, Record content remains visible in the inspector; at 1060px and below, Record becomes the fourth tab.
- **Inspector:** Quiet section-title planes introduce groups; white evidence sections follow on shared borders. Requested policy, observed posture, and portable proof remain separate and in that order.

### Output Ledger and Code Blocks

- **Material:** Code Night with Code Frost, a firm dark border, a 5px radius, and native overflow. Dark surfaces are content tools, not shell styling.
- **Rows:** Output uses a 22px rhythm with dedicated sequence, channel, and content columns plus fine dark separators. System, truncation, stderr, and violation lines receive semantic text colors while retaining their channel labels.
- **Integrity:** Never wrap or visually rewrite exact code, JSON, hashes, or event payloads merely to fit a narrow screen; preserve horizontal access to the original content.

### Dialogs and Notices

- **Dialog:** A 6px overlay with true header/body/footer regions and the Dialog Lift shadow. At 440px it becomes a full-width bottom sheet with only the top corners rounded.
- **Notice:** A full-width, line-bounded amber or red strip below the command bar. Use direct explanatory copy and an optional quiet dismiss action.
- **Empty State:** A modest 54px outlined glyph, direct heading, concise operational explanation, and one primary CTA—never fabricated ledger or evidence content.

## Do's and Don'ts

### Do:

- **Do** build hierarchy with cool-white planes, true dividers, rows, and purposeful whitespace.
- **Do** reserve indigo for action, selection, focus, active navigation, and ordered progression.
- **Do** pair every status color with an explicit label, marker shape, or both.
- **Do** use monospace for machine-authored facts and keep human guidance in the system sans.
- **Do** keep requested policy, observed posture, and portable proof visually distinct and plainly qualified as server-reported.
- **Do** preserve visible focus, 44px mobile targets, reduced-motion behavior, forced-colors support, and exact-content overflow.

### Don't:

- **Don't** turn Coop into a terminal-themed shell or a collection of equal-weight decorative cards.
- **Don't** use dark surfaces outside code, output, events, and exact JSON evidence.
- **Don't** use semantic green, amber, or red as decoration or let color become the only status cue.
- **Don't** merge requested controls with observed results or imply that server-provided evidence was independently verified.
- **Don't** treat the dashboard's three-pane composition as a universal template for unrelated tasks.
- **Don't** add gradients, oversized display type, pill-heavy chrome, or theatrical security styling unsupported by the product truth.
