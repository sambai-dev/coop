---
layout: home
title: Rookhold Sandbox
titleTemplate: Short jobs with hard limits and receipts

hero:
  name: Rookhold Sandbox
  text: Run code. Keep the boundary.
  tagline: Short Python, Node, and Bash jobs with hard limits, live output, and a receipt that records what happened.
  actions:
    - theme: brand
      text: Run the quickstart
      link: /getting-started/quickstart
    - theme: alt
      text: Read the receipt model
      link: /understand/receipts

features:
  - title: One command
    details: Run a trusted local example immediately, or point the same command at a separately operated Linux service.
  - title: Limits that end jobs
    details: Wall time, CPU, memory, process, file, and output bounds are recorded as requested and actually enforced.
  - title: One receipt per run
    details: Keep the outcome, output hashes, event-chain head, runtime digest, isolation, and requested file hashes together.
---

<div class="receipt-demo"><strong>$ rookhold run python</strong> 'print(6 * 7)'<br>42<br><br>status&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; succeeded<br>network&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; disabled<br>isolation&nbsp;&nbsp;&nbsp; gvisor-application-kernel<br>receipt&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; saved to .rookhold/runs/…/receipt.json</div>

<p class="truth-warning"><strong>Local development is not containment.</strong> On macOS, Windows, and an unisolated Linux setup, Rookhold reports <code>isolation: none</code>. Use a dedicated Linux gVisor service for untrusted code.</p>
