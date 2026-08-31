# Contribution lifecycle and evidence

Rookhold tracks technical readiness separately from repository workflow. A branch can be mechanically mergeable while its implementation is incomplete, and a fully correct contribution can remain unmerged because review or privileged integration is pending.

## The four independent states

| State | Complete means | Incomplete means |
|---|---|---|
| Implementation | Every valid case inside the declared scope satisfies the restored invariant. | A promised case, sibling path, boundary, retry, or fallback still behaves incorrectly. |
| Validation | RED evidence, focused and affected suites, required full gates, platform evidence, and exact-head CI exist. | Evidence is missing, stale, attached to an older diff, or unable to run in a trusted environment. |
| Review | Required maintainers approved the current diff and no actionable thread remains. | Approval is pending, changes are requested, or the final diff has not been reviewed. |
| Integration | The exact reviewed commit entered the protected merge/release path. | Queue, merge, trusted-branch, or release action remains pending. |

Report all four instead of describing every unmerged PR as “not done.” GitHub mergeability, review state, and CI are evidence about different layers.

## Declared scope

A PR may intentionally solve one part of a broad issue when the part is independently useful. It is scoped complete only when:

- the boundary is explicit in the title/body;
- the implementation fully handles the chosen invariant;
- the PR does not imply that excluded behavior is fixed;
- remaining work is recorded; and
- tests exercise the boundary between solved and unsolved behavior.

## Regression evidence

A regression test is strongest when it is shown to fail on unchanged source for the reported reason. Record the failing command and assertion or observable behavior before applying the fix. The repository does not require a broken commit in history, but the PR must retain the RED evidence.

For parsers, classifiers, schemas, state machines, and platform behavior, consider at least:

- missing, empty, null-like, annotated, and bare values;
- alternate valid layouts and ordering;
- accepted/rejected classifier interaction and fallthrough;
- zero, partial, and all-state transitions;
- initial, retry, cancellation, and fallback paths;
- sibling call sites that share the invariant; and
- native platform evidence when mocks cannot prove OS behavior.

## Final-diff rescan

After the last code change:

1. reread the declared scope and issue;
2. reread every actionable review comment;
3. inspect the complete final diff and sibling paths;
4. rerun focused, affected, and required full checks;
5. record the exact commit SHA in the PR; and
6. confirm CI is attached to that exact current head.

An older independent review cannot certify a newer diff. A stale PR with conflicts or no current evidence must be reproduced and rebased before it is described as ready.

## Integration discipline

Equivalent file content is not always equivalent integration history. When a target base matters, simulate the merge or rebase on the intended base and inspect the resulting diff. Do not rewrite an already approved contribution merely because it is a few unrelated commits behind; unnecessary changes can invalidate approval and retrigger risk without improving correctness.
