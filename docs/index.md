---
layout: home
title: Rookhold
titleTemplate: Short-lived code with hard limits and receipts
description: Execute short Python, Node, and Bash jobs submitted by an application, agent, or user with hard limits and a verifiable receipt.

hero:
  text: Run short-lived code with hard limits—and keep a receipt.
  tagline: Execute Python, Node, and Bash submitted by an application, agent, or user without turning your machine into the execution environment.
  image:
    src: /rook.svg
    alt: Rookhold black-square emblem
  actions:
    - theme: brand
      text: Try locally
      link: '#try'
    - theme: alt
      text: Install an SDK
      link: '#sdk'
    - theme: alt
      text: Connect an MCP host
      link: /use/mcp
---

<section class="release-candidate-note" aria-label="v0.8.0 publication status">
  <strong>v0.8.0 is current</strong>
  <span>App, CLI, Python wheel, and TypeScript tarball downloads are live. Named registry installs remain deferred.</span>
</section>

<section class="home-use-cases" aria-label="Common Rookhold use cases">
  <a href="https://github.com/sambai-dev/rookhold/tree/main/examples/llm-tool-call"><strong>Generated functions</strong><span>Run model-produced source outside the agent process.</span></a>
  <a href="https://github.com/sambai-dev/rookhold/tree/main/examples/json-transform"><strong>JSON transforms</strong><span>Apply user-defined code to structured input.</span></a>
  <a href="https://github.com/sambai-dev/rookhold/tree/main/examples/evaluator"><strong>Code grading</strong><span>Bound hidden-test evaluation and keep the record.</span></a>
</section>

<section class="home-demo" aria-labelledby="demo-heading">
  <div class="home-demo-copy">
    <h2 id="demo-heading">One command to a real result.</h2>
    <p>With no endpoint configured, the Rookhold app starts a temporary loopback service, runs trusted code, saves the receipt, and removes the service state. It reports the weak local posture plainly.</p>
    <a class="home-text-link" href="./getting-started/quickstart">Open the two-minute quickstart <span aria-hidden="true">→</span></a>
  </div>
  <div class="terminal-stage" role="img" aria-label="Local trusted-code run reporting host networking and no isolation">
    <div class="terminal-head"><span>rookhold app</span><span class="local-caution">local · trusted code only</span></div>
    <div class="terminal-line"><span class="prompt">›</span><span>rookhold run python "print(6 * 7)"</span></div>
    <div class="terminal-event"><span>01</span><strong>job completed</strong><span>42</span></div>
    <div class="terminal-event"><span>02</span><strong>network</strong><span>host</span></div>
    <div class="terminal-event"><span>03</span><strong>isolation</strong><span>none</span></div>
    <div class="terminal-receipt"><span>receipt</span><code>.rookhold/runs/…/receipt.json</code><b>saved</b></div>
  </div>
</section>

<section id="try" class="download-section" aria-labelledby="try-heading">
  <div class="section-heading">
    <h2 id="try-heading">Try the Rookhold app.</h2>
    <p>The complete bundle contains the unified app, local service, remote client, MCP adapter, verifier, and setup templates.</p>
  </div>
  <div class="download-grid">
    <a class="download-option" aria-label="Download the Rookhold app for 64-bit Windows" href="https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-x86_64-pc-windows-msvc.zip"><strong>Windows</strong></a>
    <a class="download-option" aria-label="Download the Rookhold app for Apple Silicon Mac" href="https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-aarch64-apple-darwin.tar.gz"><strong>Mac</strong></a>
    <a class="download-option" aria-label="Download the Rookhold app for Linux x86_64" href="https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-x86_64-unknown-linux-musl.tar.gz"><strong>Linux</strong></a>
  </div>
  <p class="download-compatibility">Windows and Linux: 64-bit Intel or AMD. Mac: Apple silicon. Mac and Linux users run <code>chmod +x</code> once after extracting.</p>
</section>

<section id="sdk" class="sdk-section" aria-labelledby="sdk-heading">
  <div class="section-heading">
    <h2 id="sdk-heading">Add Rookhold to an application.</h2>
    <p>Install the exact v0.8.0 SDK release assets. They submit jobs to an endpoint and do not create the guarded Linux boundary by themselves.</p>
  </div>
  <div class="sdk-layout">
    <div class="install-command"><span>Python</span><code>pip install https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0-py3-none-any.whl</code><a href="./use/python">Python guide →</a></div>
    <div class="install-command"><span>TypeScript</span><code>npm install https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0.tgz</code><a href="./use/typescript">TypeScript guide →</a></div>
    <p>Named PyPI and npm installs are deferred while maintainer registry accounts are activated; v0.8.0 ships the exact packages as verified release assets.</p>
  </div>
</section>

<section class="client-section" aria-labelledby="client-heading">
  <div class="section-heading">
    <h2 id="client-heading">Already have a Rookhold server?</h2>
    <p>The standalone client is the smaller human and MCP interface. It does not include the service or local-run workflow.</p>
    <div class="home-demo-actions">
      <a class="home-text-link" href="./use/cli">Client guide <span aria-hidden="true">→</span></a>
      <a class="home-text-link" href="./use/mcp">MCP setup <span aria-hidden="true">→</span></a>
    </div>
  </div>
  <div class="client-links" aria-label="Standalone client downloads">
    <a href="https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-cli-x86_64-pc-windows-msvc.exe">Windows client</a>
    <a href="https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-cli-aarch64-apple-darwin">Mac client</a>
    <a href="https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-cli-x86_64-unknown-linux-gnu">Linux client</a>
  </div>
</section>

<section class="boundary-section" aria-labelledby="boundary-heading">
  <div><h2 id="boundary-heading">The local demo is not the security boundary.</h2></div>
  <div class="boundary-copy">
    <p>For untrusted code, operate Rookhold on a dedicated Linux x86_64 host with the pinned gVisor provider. Local development on Windows, macOS, or an unisolated Linux setup reports <code>isolation: none</code>.</p>
    <p>Requested policy, observed controls, and portable proof stay separate. A run fails admission when the service cannot satisfy its required isolation class.</p>
    <a class="home-text-link" href="./getting-started/first-secure-deployment">Build the secure boundary <span aria-hidden="true">→</span></a>
  </div>
</section>

<section class="faq-section" aria-labelledby="faq-heading">
  <div class="section-heading"><h2 id="faq-heading">Choose the right surface.</h2></div>
  <div class="faq-list">
    <details><summary>Which download should I use first?</summary><p>Use the complete Rookhold app bundle for your operating system. It includes the zero-configuration local path and the server.</p></details>
    <details><summary>When should I use the standalone client?</summary><p>Use it only when you already have a Rookhold endpoint and want the smaller operator or MCP interface.</p></details>
    <details><summary>Do the SDKs include the sandbox service?</summary><p>No. They submit to an endpoint. The secure boundary is a separately operated Linux service.</p></details>
    <details><summary>Can I safely run untrusted code on my laptop?</summary><p>No. The local mode is explicitly unisolated. Use the dedicated Linux gVisor deployment for mutually untrusted code.</p></details>
  </div>
</section>

<section class="final-cta" aria-label="Try Rookhold locally">
  <h2>Run one trusted local job, then decide where the boundary belongs.</h2>
  <a href="#try">Try Rookhold locally <span aria-hidden="true">↓</span></a>
</section>
