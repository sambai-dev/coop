---
name: "Coop"
description: "Chalk command planes around a carbon run transcript, with cobalt intent and truthful semantic state."
colors:
  workbench: "#fdfeff"
  control-white: "#ffffff"
  surface: "#f9fafa"
  surface-raised: "#f5f6f9"
  border: "#e1e4e9"
  border-strong: "#c8cdd6"
  text: "#111827"
  text-muted: "#5e6675"
  text-faint: "#7a8392"
  kernel: "#1e55e6"
  kernel-strong: "#1647ca"
  selection: "#edf2ff"
  focus: "#1e55e6"
  carbon: "#12171e"
  carbon-raised: "#171d25"
  carbon-soft: "#1b222c"
  carbon-border: "#29323e"
  carbon-text: "#e7ebf1"
  carbon-muted: "#9da7b5"
  carbon-accent: "#79a0ff"
  code-ink: "#1e3a8a"
  healthy: "#198754"
  warning: "#a76400"
  danger: "#cc3340"
  cancelled: "#667085"
typography:
  headline:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "19px"
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
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.45
    letterSpacing: "normal"
  label:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "11px"
    fontWeight: 650
    lineHeight: 1.45
    letterSpacing: "normal"
  micro:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "10px"
    fontWeight: 400
    lineHeight: 1.4
    letterSpacing: "normal"
  mono:
    fontFamily: "SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: "22px"
    letterSpacing: "normal"
  transcript:
    fontFamily: "SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace"
    fontSize: "11.5px"
    fontWeight: 400
    lineHeight: 1.55
    letterSpacing: "normal"
  run-identifier:
    fontFamily: "SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace"
    fontSize: "13px"
    fontWeight: 600
    lineHeight: 1.3
    letterSpacing: "-0.01em"
rounded:
  none: "0"
  micro: "3px"
  control: "4px"
  overlay: "6px"
  circle: "50%"
components:
  button-primary:
    backgroundColor: "{colors.kernel}"
    textColor: "{colors.control-white}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "38px"
  button-primary-hover:
    backgroundColor: "{colors.kernel-strong}"
    textColor: "{colors.control-white}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "38px"
  button-queue:
    backgroundColor: "{colors.kernel}"
    textColor: "{colors.control-white}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "48px"
  button-secondary:
    backgroundColor: "{colors.control-white}"
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
  context-chip:
    backgroundColor: "{colors.control-white}"
    textColor: "{colors.text-muted}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "6px 9px"
    height: "38px"
  input:
    backgroundColor: "{colors.control-white}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    padding: "8px 10px"
    height: "40px"
  dock-tab-active:
    backgroundColor: "transparent"
    textColor: "{colors.kernel}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    padding: "10px 16px"
    height: "56px"
  code-editor:
    backgroundColor: "{colors.control-white}"
    textColor: "{colors.code-ink}"
    typography: "{typography.mono}"
    rounded: "{rounded.control}"
    padding: "9px 10px"
  history-row-selected:
    backgroundColor: "{colors.selection}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "8px 10px"
    height: "62px"
  transcript-stage:
    backgroundColor: "{colors.carbon}"
    textColor: "{colors.carbon-text}"
    typography: "{typography.transcript}"
    rounded: "{rounded.none}"
    padding: "14px 18px 14px 8px"
    height: "78px"
  result-section:
    backgroundColor: "{colors.surface-raised}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "14px"
---

# Design System: Coop

## Overview

**Creative North Star: "The Chalk-and-Carbon Execution Desk"**

Coop is an exacting operations surface built from two materials with different responsibilities. Chalk-white command planes hold intent, configuration, history, and proof; a deep carbon well holds the truthful chronological run transcript. The contrast makes execution the focal object without dressing the product as a novelty terminal.

Cobalt marks deliberate operator intent: queueing, selection, focus, active navigation, and transcript structure. Green, amber, red, and slate report real execution state. One-pixel seams, compact square controls, restrained type, and server-qualified language keep the system direct and credible.

The dashboard expresses the world as a Compose/History dock, dominant transcript, and contextual Result & record sheet. That composition belongs to the run workflow rather than every future Coop surface. The durable system is the material hierarchy—chalk around carbon—plus the ordered move from requested intent to accepted policy, execution, result, and portable proof.

**Key Characteristics:**

- Chalk command and proof planes surrounding one dominant carbon execution well.
- Intent Cobalt reserved for operator action, focus, selection, and chronological structure.
- Semantic colors paired with explicit labels, markers, and server-reported facts.
- Compact square controls and one-pixel seams instead of decorative card chrome.
- Humanist system sans for operation; monospace for source, identifiers, time, and evidence.
- Responsive movement from simultaneous context to one focused Compose, History, Run, or Result surface.

## Colors

The palette uses bright chalk neutrals for command surfaces and a layered blue-black carbon family for the execution transcript, joined by one cobalt interaction voice.

### Primary

- **Intent Cobalt** (`kernel`): Primary actions, active tabs, focus, chronological transcript accents, and the selected History seam.
- **Pressed Cobalt** (`kernel-strong`): Primary-button hover and stronger action feedback.
- **Carbon Cobalt** (`carbon-accent`): Readable active labels and stage headings inside the carbon well.
- **Selection Wash** (`selection`): The quiet background of a selected History row; it works with, never replaces, the one-pixel cobalt seam.
- **Focus Cobalt** (`focus`): The global three-pixel visible focus outline.

### Secondary

- **Outcome Green** (`healthy`): Connected, succeeded, and successful completion states.
- **Attention Amber** (`warning`): Active work, reconnecting, integrity cautions, and non-terminal attention.
- **Boundary Red** (`danger`): Failure, unavailable or weak runtime posture, destructive actions, and blocking errors.
- **Cancelled Slate** (`cancelled`): Neutral terminal outcomes that are neither success nor failure.

### Neutral

- **Workbench Chalk** (`workbench`): Command-bar and dock-tab plane.
- **Control White** (`control-white`): Inputs, chips, code editor, result metrics, and exact record payloads.
- **Composer Chalk** (`surface`): The Compose/History dock body.
- **Record Paper** (`surface-raised`): The contextual Result & record sheet and its evidence sections.
- **Hairline Seam** (`border`): Plane boundaries, rows, tabs, metrics, and evidence sections.
- **Control Edge** (`border-strong`): Inputs, buttons, keycaps, and true overlays.
- **Carbon Ink** (`text`): Primary text on chalk planes.
- **Graphite** (`text-muted`): Secondary copy, inactive navigation, and control metadata.
- **Faint Blue-Gray** (`text-faint`): Hints, qualifications, and low-priority context.
- **Execution Carbon** (`carbon`): The dominant transcript canvas.
- **Raised Carbon** (`carbon-raised`): Buttons, blank glyphs, and controls inside the execution well.
- **Soft Carbon** (`carbon-soft`): Hover and locally emphasized carbon controls.
- **Carbon Seam** (`carbon-border`): Transcript stages, run header, tabs, and internal carbon boundaries.
- **Carbon Frost** (`carbon-text`): Primary text on the execution well.
- **Carbon Ash** (`carbon-muted`): Timestamps, supporting metadata, and inactive controls on carbon.
- **Source Ink** (`code-ink`): Editable source text on the white composer editor.

**The Cobalt Means Intent Rule.** Use cobalt for deliberate action, focus, current navigation, selection seams, and transcript structure; do not use it as ambient decoration.

**The Semantics Mean State Rule.** Green, amber, red, and slate must correspond to an actual reported state and remain paired with text or marker shape.

**The Chalk Around Carbon Rule.** Command, composition, and proof stay on chalk planes; the chronological execution transcript owns the carbon well.

## Typography

**Display Font:** System UI sans (with ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif)

**Body Font:** System UI sans (with ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif)

**Label/Mono Font:** System monospace (with SFMono-Regular, Consolas, Liberation Mono, Menlo, monospace)

**Character:** The sans is compact, human, and operational. Monospace identifies information whose exact shape matters; neither family is used for theatrical terminal styling.

### Hierarchy

- **Headline** (700, 19px, 1.45): Product identity and the largest operational statement.
- **Title** (700, 15px, 1.45): Composer, History, Result & record, and major section titles.
- **Body** (400, 14px, 1.45): Controls, instructions, and primary explanatory copy.
- **Label** (650, 11px, 1.45): Field labels, transcript stage labels, section captions, and compact operational headings.
- **Micro** (400, 10px, 1.4): Qualifications, shortcuts, helper copy, and low-priority metadata.
- **Run Identifier** (600, 13px, 1.3): The selected job ID in the carbon run header.
- **Transcript Mono** (400, 11.5px, 1.55): Requested source, accepted policy, execution facts, and chronological payloads.
- **Ledger Mono** (400, 12px, 22px line height): Virtualized stdout and ordered event rows.

**The Machine Facts Are Monospace Rule.** Use monospace for source, IDs, timestamps, limits, hashes, events, output, JSON, and exact values; keep actions and explanations in the system sans.

**The Density Comes From Rhythm Rule.** Do not introduce marketing-scale type. Establish priority through weight, alignment, plane contrast, and four-pixel rhythm.

## Layout

The shipped dashboard uses a fixed command plane (68px) over a three-part run workspace: a 292px Compose/History dock, a flexible carbon transcript, and a closeable 310px Result & record sheet. The Queue run action anchors the bottom of the dock while requested policy remains visible above the run header. At 1240px the dock and sheet tighten to 278px and 294px.

Spacing follows a four-pixel base without a named token scale: 4–8px for tight relationships, 10–12px inside dense controls, 14–18px within stages and sections, and 20px at major plane edges. Adjacent regions meet on one-pixel seams rather than gutters or card gaps.

At 1060px and below, the dock, transcript, and result sheet become mutually focused surfaces under a 118px command bar. At 720px the bar becomes 166px, the transcript collapses from time / marker / content columns to marker / content, and primary controls maintain at least 44px targets. At 440px the brand name yields space, History drops its separate column header, and record metadata stacks. Requested policy still precedes observed posture and portable proof.

**The Composition Stays With the Run Rule.** Preserve the dock → transcript → result anatomy for this run workflow, but do not force those widths or three regions onto unrelated Coop surfaces.

**The Sequence Never Reflows Rule.** Responsive layout may change which plane is visible, never the order from requested intent through accepted policy, execution, outcome, and proof.

## Elevation & Depth

The world is flat and material, not floating. Chalk and carbon planes are separated by tone and one-pixel seams; the dock, transcript, policy ribbon, result sheet, and ordinary actions have no ambient shadow. Depth appears only for real overlays.

### Shadow Vocabulary

- **History Selection Seam** (`inset 1px 0 0 var(--kernel)`): The final current-row marker; it is a one-pixel seam, not an elevation effect.
- **Context Overlay** (`0 16px 38px rgba(0, 0, 0, .16)`): Authenticated connection details opened above the command bar.
- **Toast Lift** (`0 12px 28px rgba(31, 42, 61, .18)`): Temporary feedback at the viewport edge.

**The Seam Before Shadow Rule.** If an element does not overlap another plane, separate it with tone, spacing, or a one-pixel border before considering shadow.

## Shapes

Coop uses square planes, one-pixel seams, and compact controls with a restrained four-pixel radius. Three-pixel corners belong to keycaps and nested output wells; six-pixel corners are reserved for true overlays and the blank-state glyph. Status dots and transcript markers remain circular.

**The Square Desk Rule.** Composer, History, transcript, policy ribbon, and result sheet meet edge to edge; do not turn them into detached rounded cards.

**The One-Pixel Selection Rule.** A selected History row uses Selection Wash plus exactly one cobalt pixel on its leading edge. Do not restore the old heavy rail.

## Components

### Buttons

- **Shape:** Four-pixel corners with a 38px minimum height and 8px × 12px padding; coarse pointers raise interactive targets to at least 44px.
- **Primary:** Intent Cobalt with Control White and Pressed Cobalt on hover. The anchored Queue run variant is 48px high and spans the dock footer.
- **Secondary:** Control White or Raised Carbon, depending on its plane, with a visible border and primary text for concrete utility actions.
- **Quiet:** Transparent, borderless at rest, and visually subordinate for Close, History, copy, and disclosure actions.
- **Motion / Focus:** Color changes use 150ms and presses use 120ms with a 0.98 scale. Keyboard focus uses the global outline; reduced-motion preference removes transitions and transforms.

### Context Chips

- **Style:** Compact 38px controls with four-pixel corners, white fill, one-pixel edge, and a dot-plus-label structure.
- **State:** Gray is unknown, green is ready, amber is caution, and red is error or weak boundary. The text value remains visible beside the dot.

### Inputs / Fields

- **Style:** Control White, a four-pixel radius, 40px minimum height, and a cool one-pixel edge. Labels are 11px and semibold; help text is 10px and quiet.
- **Focus:** The field edge shifts to cobalt while the global three-pixel focus outline remains available.
- **Code Editor:** A 28px numbered gutter and white source plane share a four-pixel shell. Source Ink uses a 21px line rhythm; focus encloses the whole editor rather than only the textarea.

### Compose / History Navigation

- **Tabs:** Two equal 56px dock tabs on Workbench Chalk. The active tab uses cobalt text and a two-pixel underline inset 16px from both sides.
- **History Rows:** Compact table-like rows are at least 62px high. The selected row uses Selection Wash and the final one-pixel cobalt seam; hover remains a lighter cobalt wash.

### Requested Policy Ribbon

- **Structure:** A minimum 56px chalk strip between command context and the run header.
- **Language:** “Requested policy” and “not a guarantee” remain adjacent; the compact monospace summary may truncate on desktop but wraps on mobile.

### Chronological Transcript

- **Structure:** The signature component is a vertical sequence of intent, accepted policy, execution, output, system events, completion, and proof. Desktop stages use 110px time, 28px marker, and flexible content columns; each stage is at least 78px high.
- **Material:** Execution Carbon with Carbon Frost, Carbon Ash metadata, Carbon Cobalt headings, circular markers, and a one-pixel vertical connector.
- **Payload:** Monospace content preserves exact values. The nested output well uses a deeper carbon plane, 22px rows, and native scrolling.
- **Responsive:** Below 720px, time moves above content while the marker and vertical sequence remain intact.

### Result & Record Sheet

- **Structure:** A 310px contextual Record Paper sheet with a sticky 76px heading and explicit Close action; it narrows to 294px before becoming a focused surface.
- **Content:** Result metrics, observed posture, requested policy, record metadata, JSON, and exact-download actions stack on shared seams.
- **Truth:** The heading and evidence copy stay visibly qualified as server-reported. Closing the sheet returns the transcript to full width.

### Notices, Popovers, and Empty States

- **Notice:** A line-bounded amber or red strip below the command bar with direct text and a quiet dismiss action.
- **Popover:** Connection context is a true overlay and may use the Context Overlay shadow.
- **Empty State:** The carbon well uses a restrained raised-carbon glyph, one direct heading, short explanation, and one cobalt Compose action.

## Do's and Don'ts

### Do:

- **Do** make the chronological transcript the dominant visual and interaction surface.
- **Do** keep Compose, History, requested policy, and Result & record on chalk planes around the carbon well.
- **Do** reserve cobalt for intent, current navigation, visible focus, transcript structure, and the one-pixel History seam.
- **Do** pair semantic state color with explicit labels, markers, and server-reported facts.
- **Do** preserve the visual separation between requested intent, accepted policy, observed result, and portable proof.
- **Do** retain visible focus, 44px coarse-pointer targets, reduced motion, forced colors, 320px reflow, and exact-content scrolling.

### Don't:

- **Don't** revive equal-weight dashboard panels, decorative card grids, or the old queue / output / inspector hierarchy.
- **Don't** style the carbon transcript as a fake terminal window with chrome, traffic-light controls, or novelty prompts.
- **Don't** spread dark material across the Compose dock, command bar, or Result & record sheet.
- **Don't** thicken the selected History indicator beyond its final one-pixel cobalt seam.
- **Don't** merge requested policy with observed posture or imply that server-provided proof was independently verified.
- **Don't** add gradients, glass, pill-heavy controls, decorative rasters, or theatrical security styling.
