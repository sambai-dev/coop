## Declared scope

<!-- State the independently useful behavior this PR promises. Do not restate a broad issue unless this PR fully solves it. -->

## Root invariant

<!-- Name the contract or state that first became invalid and that this change restores. -->

## Reproduction and RED evidence

<!-- Write "RED: ..." with the command and failure reason observed on unchanged source. For non-regression work, write "Not applicable because ...". -->

## Changes

<!-- Summarize the focused implementation and affected sibling paths. -->

## Adversarial coverage

<!-- Check every applicable case and add domain-specific cases. If none apply, explain "Not applicable because ...". -->

- [ ] missing, empty, null-like, or annotated inputs
- [ ] alternate valid layouts or orderings
- [ ] zero, partial, all-state, retry, and fallback boundaries
- [ ] sibling classifiers, handlers, or call sites with the same invariant
- [ ] platform-specific behavior

## Validation on final head

<!-- Re-run after the last code change. Include the exact current commit and attached CI, not evidence from an older diff. -->

- Head: <!-- commit SHA -->
- Checks: <!-- focused test, affected suite, lint/type/format, full required CI, platform evidence -->

## Non-goals and remaining work

<!-- State intentionally excluded behavior and where remaining work is recorded. Do not write bare "N/A". -->

## Safety and compatibility

<!-- Effects on isolation, tenant boundaries, replayability, API/evidence compatibility, deployment, and rollback. -->

## Completion state

<!-- Keep technical completion distinct from repository lifecycle state. -->

- Implementation: <!-- complete or incomplete for the declared scope, with reason -->
- Validation: <!-- complete on this head or pending, with reason -->
- Review: <!-- pending, approved, or changes requested -->
- Integration: <!-- pending protected merge, merged, or blocked, with reason -->
