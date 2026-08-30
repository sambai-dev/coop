# Coop v0.5 design QA

## Evidence

- Approved source: `.impeccable/mocks/chalk-carbon-execution-desk.png`
- Source dimensions: 1487 × 1058 px
- Implemented surface: `crates/coop-server/src/dashboard.html`
- Primary implementation capture: `.impeccable/review/desktop.png`
- Implementation dimensions: 1440 × 1024 px at 1440 × 1024 CSS px, DPR 1
- Responsive captures: `.impeccable/review/user-1060.png`, `.impeccable/review/mobile.png`, `.impeccable/review/mobile-440.png`, `.impeccable/review/mobile-queue.png`, and `.impeccable/review/mobile-record.png`
- Compared state: authenticated tenant, selected successful Python run, output transcript visible, result-and-record sheet open

## Visual comparison

The implementation keeps the approved composition: a chalk-white compose/history dock, a carbon chronological transcript, a narrow requested-policy ribbon, and a contextual white result sheet. The implementation intentionally uses the real Coop response model instead of the mock's illustrative fields. It preserves the reference's compact operator density, electric-blue action color, status accents, square geometry, and monospace evidence language.

The first implementation left the proof stage below the primary desktop fold because the output viewport and transcript gaps were too tall. The transcript was tightened and the output viewport bounded so intent, policy, execution, output, system events, completion, and proof now read as one sequence. An intermediate capture was unstyled after a stale CSP hash; the exact inline-style hash was updated before the valid final capture.

The independent finish reviewer rejected three stale v0.4 evidence files, so the final desktop-empty, mobile-History, and mobile-record states were recaptured from the rebuilt v0.5 artifact. The full review then found one craft-floor issue: the selected History row used a 2px cobalt side stripe. It was reduced to a one-pixel seam, the CSP hash was regenerated, and the affected 390 × 844 History capture was repeated. The reviewer scored the fix resolved with no regressions and returned `ship`.

No unresolved P0, P1, or P2 visual differences remain.

## Interaction checks

- API-key apply and authenticated context rendering
- Compose/History APG tabs, including arrow-key and focus behavior
- language and six-class minimum-isolation selection
- code, standard input, requested limits, and network-request controls
- queue run, select from history, and top-bar run switcher
- output/events tabs and follow-tail behavior
- result-sheet close/reopen with focus returned to the invoker
- copy ID/output/events and exact result/attestation download controls
- cancel confirmation scoped to the selected job ID
- reconnectable selected-run rendering and empty/error states

Browser console checks reported no errors in the tested states.

## Responsive and accessibility checks

- 1440 px: three-pane execution desk
- 1060 px: focused workspace with preserved run and sheet access
- 720 px: focused single-surface navigation
- 440 px and 390 px: compact dock, run, and record views
- 320 CSS px: no document-level horizontal overflow
- keyboard-visible focus, skip links, labels, fieldset/legend grouping, live status regions, tab semantics, and touch/coarse-pointer targets are present
- Axe Core 4.13.0 was run against every normally hidden dashboard state after exposing it in a static DOM: 0 WCAG 2.0 A/AA, 2.1 AA, or 2.2 AA violations and 31 passed rules
- Axe could not compute the hidden skip-link contrast in the static DOM. Manual check: white text on `#111827` is approximately 17.7:1, above WCAG AA and AAA thresholds

## Result

`passed` — independent finish disposition: `ship`
