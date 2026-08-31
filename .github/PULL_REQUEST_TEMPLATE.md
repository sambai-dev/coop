## Contribution tier

<!-- Write Tier A (docs/examples/integrations), Tier B (SDK/CLI/public API), or Tier C (executor/auth/storage/receipts/isolation). -->

## Declared scope

<!-- State the independently useful behavior this PR promises. Do not restate a broad issue unless this PR fully solves it. -->

## Root invariant

<!-- Tier C required. Tier B when repairing a regression. Tier A may remove this section. -->

## Reproduction and RED evidence

<!-- Tier C required. Tier B when repairing a regression. Tier A may remove this section. -->

## Changes

<!-- Summarize the focused implementation and affected sibling paths. -->

## Adversarial coverage

<!-- Tier C required. Tier B should list relevant API edge cases. Tier A may remove this section. -->

- [ ] missing, empty, null-like, or annotated inputs
- [ ] alternate valid layouts or orderings
- [ ] zero, partial, all-state, retry, and fallback boundaries
- [ ] sibling classifiers, handlers, or call sites with the same invariant
- [ ] platform-specific behavior

## Validation

<!-- Tier A: relevant formatter and example/configuration check. Tier B: tests, types, package smoke. Tier C: exact head and all required gates. -->

- Head: <!-- commit SHA -->
- Checks: <!-- focused test, affected suite, lint/type/format, full required CI, platform evidence -->

## Non-goals and remaining work

<!-- State intentionally excluded behavior and where remaining work is recorded. Do not write bare "N/A". -->

## Safety and compatibility

<!-- Tier B and C required. Tier A may remove this section. -->

## Completion state

<!-- Tier C required. Other tiers may remove this section. -->

- Implementation: <!-- complete or incomplete for the declared scope, with reason -->
- Validation: <!-- complete on this head or pending, with reason -->
- Review: <!-- pending, approved, or changes requested -->
- Integration: <!-- pending protected merge, merged, or blocked, with reason -->
