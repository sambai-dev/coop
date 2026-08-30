# Mobbin UI research — 2026-08-30

## Current-state diagnosis

The existing console is functionally strong but visually reads as a generated developer demo: near-black everywhere, terminal-style monospace used as identity, many small uppercase labels, repeated pills, outlined panels nested inside panels, and security posture rendered as decorative chrome. The run flow is present, but the hierarchy makes connection metadata, aggregate counters, the run queue, output, and evidence compete at the same weight. On mobile, the whole connection and authority matrix pushes the selected run far below the fold.

## Reference patterns worth carrying forward

- **Databricks run detail:** a dense work table owns the center while one calm side inspector answers the selected-run questions. Actions are placed with the run rather than scattered across global chrome. Source: https://mobbin.com/screens/73a15e11-aec8-40ea-ab1b-d9946fbb99e7
- **Hume AI job detail:** identity, state, timeline, request configuration, and downloadable outputs read as one evidence document instead of a dashboard of unrelated cards. Source: https://mobbin.com/flows/509d8356-b2e3-42f8-9abb-55960509adbe
- **Snowflake notebook:** the creation surface is quiet and task-first; code owns the workspace while language and run controls remain compact and local. Source: https://mobbin.com/screens/400593c7-f898-4531-b021-8f26a69fcc31
- **GitHub security log:** security history is a legible grouped ledger with direct filtering and restrained status treatment, not a wall of glowing badges. Source: https://mobbin.com/screens/0a54f53c-2276-4628-92cc-a170af40fb43
- **Adaline monitoring:** a persistent list/detail split lets operators keep spatial context while inspecting one run. Source: https://mobbin.com/screens/2207d89d-a8ef-43af-abe9-172896657854

## Direction constraints

1. Use a light neutral base suited to an operator working during normal office or incident-room lighting; reserve dark material for code/output only.
2. Make the run list, selected execution, and proof record three levels of one workflow, not independent cards.
3. Replace pill overload with plain text, compact status marks, dividers, and row alignment.
4. Keep monospace for IDs, code, hashes, and logs; use a workhorse UI face everywhere else.
5. Keep authentication and granted authority available but collapsed into a concise connection inspector after the key is applied.
6. Make success/failure state redundant through icon, label, and color; color is never the only signal.
7. On narrow screens, turn the list/detail split into an explicit back-stack so the selected run starts near the top rather than below the entire ledger.

## Grounded visual candidates

1. **Three-pane operations desk:** compact queue, wide execution workspace, pinned evidence inspector; closest to Databricks/Snowflake and the strongest task match.
2. **Flight-test telemetry sheet:** one horizontal run strip followed by live instruments and a signed test record; clear but more metaphorical.
3. **Incident-command workbook:** queue as incident index, output as working log, evidence as the closed incident record.
4. **Security audit ledger:** GitHub-like grouped run history opening into a document-first evidence record.
5. **Notebook-to-proof workspace:** code creation stays visible beside output, then folds into the terminal receipt.
6. **Execution dossier:** a bright official run document with a narrow queue, central event ledger, and pinned proof folio; every datum has one stable place.
7. **Release-control manifest:** runs read like release candidates moving from requested policy to observed controls to signed artifacts.

The category-default dark terminal dashboard and its predictable opposite, a colorful consumer-card dashboard, are deliberately excluded.

## Seed decision

Impeccable seed `9cf53bab` assigned candidate 6, **Execution dossier**. The grounded pick is candidate 1, **Three-pane operations desk**. Catalog challengers were declined because none beat the grounded candidates on both operator identification and product clarity:

- Night-flight six-pack: operator identification holds, but round gauges misrepresent event and evidence data. Kept discipline: fixed cross-check reading order.
- Jet-age ticket wallet: durable state and non-destructive cancellation are useful, but the paper metaphor slows routine monitoring. Kept discipline: cancelled evidence remains visible.
- Cracktro queue: depth ranking is clear, but it recreates the rejected dark-terminal costume. Kept discipline: one unmistakable foreground task.
- Miura sheet: linked policy-to-evidence propagation is compelling, but the folding system obscures familiar controls. Kept discipline: linked fields should visually reveal causality.
- Mecha crisis wall: urgency is legible, but permanent alarm language destroys trust calibration. Kept discipline: reserve alarm color for real exceptions.
- Cloud quarry: ambitious material character, but it does not identify the operator audience or execution mechanism. Kept discipline: create one memorable product-specific transition.
