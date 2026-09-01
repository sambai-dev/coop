---
layout: home
title: Rookhold
titleTemplate: Controlled code execution for AI agents
description: Download the Rookhold CLI and connect Claude Code, OpenCode, Hermes, or another MCP host to a controlled execution service.

hero:
  text: Run agent code behind a boundary you control.
  tagline: One downloadable CLI connects your app or agent to short Python, Node, and Bash jobs with hard limits, bounded live output, and a verifiable receipt.
  image:
    src: /rook.svg
    alt: Rookhold black-square emblem
  actions:
    - theme: brand
      text: Download the CLI
      link: '#download'
    - theme: alt
      text: Connect an MCP host
      link: /use/mcp
---

<section class="home-proof" aria-label="Release facts">
  <p><strong>Current stable</strong><span>v0.7.1</span></p>
  <p><strong>Agent interface</strong><span>MCP over stdio</span></p>
  <p><strong>Production boundary</strong><span>gVisor on Linux x86_64</span></p>
</section>

<section class="home-demo" aria-labelledby="demo-heading">
  <div class="home-demo-copy">
    <h2 id="demo-heading">One file for you and your agent.</h2>
    <p>The normal command opens Rookhold's operator CLI. The <code>mcp-server</code> argument turns that same executable into a concurrent stdio MCP server with four narrow execution tools.</p>
    <div class="home-demo-actions">
      <a class="home-text-link" href="./use/cli">Open the CLI guide <span aria-hidden="true">→</span></a>
      <a class="home-text-link" href="./use/mcp">Open the MCP guide <span aria-hidden="true">→</span></a>
    </div>
  </div>
  <div class="terminal-stage" role="img" aria-label="Rookhold CLI showing a successful bounded Python job and saved receipt">
    <div class="terminal-head"><span>rookhold-cli</span><span>connected · gvisor</span></div>
    <div class="terminal-line"><span class="prompt">›</span><span>/run python "print(6 * 7)"</span></div>
    <div class="terminal-event"><span>01</span><strong>policy accepted</strong><span>network disabled · 2s wall</span></div>
    <div class="terminal-event"><span>02</span><strong>job completed</strong><span>42</span></div>
    <div class="terminal-receipt"><span>receipt</span><code>.rookhold/runs/…/receipt.json</code><b>succeeded</b></div>
  </div>
</section>

<section id="download" class="download-section" aria-labelledby="download-heading">
  <div class="section-heading">
    <h2 id="download-heading">Download Rookhold.</h2>
    <p>Choose your computer. Each option is one ready-to-run CLI file.</p>
  </div>
  <div class="download-grid">
    <a class="download-option" aria-label="Download Rookhold for Windows" href="https://github.com/sambai-dev/rookhold/releases/download/v0.7.1/rookhold-cli-x86_64-pc-windows-msvc.exe">
      <strong>Windows</strong>
    </a>
    <a class="download-option" aria-label="Download Rookhold for Mac" href="https://github.com/sambai-dev/rookhold/releases/download/v0.7.1/rookhold-cli-aarch64-apple-darwin">
      <strong>Mac</strong>
    </a>
    <a class="download-option" aria-label="Download Rookhold for Linux" href="https://github.com/sambai-dev/rookhold/releases/download/v0.7.1/rookhold-cli-x86_64-unknown-linux-gnu">
      <strong>Linux</strong>
    </a>
  </div>
</section>

<section class="mcp-section" aria-labelledby="mcp-heading">
  <div class="section-heading">
    <h2 id="mcp-heading">Connect any local MCP host.</h2>
    <p>Rookhold keeps the endpoint, credential, allowed languages, and minimum isolation outside the model's tool arguments.</p>
  </div>
  <div class="mcp-layout">
    <div class="mcp-code" aria-label="Generic MCP configuration example">
      <div class="mcp-code-head"><span>mcp.json</span><span>stdio</span></div>
      <pre><code>{
  "mcpServers": {
    "rookhold": {
      "command": "rookhold-cli",
      "args": ["mcp-server"]
    }
  }
}</code></pre>
    </div>
    <ol class="mcp-steps">
      <li><span>01</span><div><strong>Download one file</strong><p>Put <code>rookhold-cli</code> on PATH or use its absolute path.</p></div></li>
      <li><span>02</span><div><strong>Set operator policy</strong><p>Export the endpoint, scoped key, language allowlist, and required isolation class.</p></div></li>
      <li><span>03</span><div><strong>Register and check</strong><p>Launch <code>mcp-server</code>, then confirm the four live Rookhold tools in your host.</p></div></li>
    </ol>
  </div>
  <div class="host-links" aria-label="Supported host guides">
    <a href="https://github.com/sambai-dev/rookhold/blob/main/integrations/claude-code/mcp.json">Claude Code</a>
    <a href="https://github.com/sambai-dev/rookhold/blob/main/integrations/opencode/opencode.snippet.json">OpenCode</a>
    <a href="https://github.com/sambai-dev/rookhold/blob/main/integrations/hermes/config.snippet.yaml">Hermes</a>
    <a href="./use/mcp">Generic MCP</a>
  </div>
</section>

<section class="boundary-section" aria-labelledby="boundary-heading">
  <div>
    <h2 id="boundary-heading">The website is hosted. The executor is yours.</h2>
  </div>
  <div class="boundary-copy">
    <p>Vercel serves these public docs. Download links point directly to immutable GitHub Releases assets. Vercel does not run Rookhold's privileged execution backend.</p>
    <p>For untrusted code, operate Rookhold on a dedicated Linux x86_64 host with the pinned gVisor provider. Local development on Windows, macOS, or an unisolated Linux setup reports <code>isolation: none</code>.</p>
    <a class="home-text-link" href="./getting-started/first-secure-deployment">Build the secure boundary <span aria-hidden="true">→</span></a>
  </div>
</section>

<section class="faq-section" aria-labelledby="faq-heading">
  <div class="section-heading"><h2 id="faq-heading">The short answers.</h2></div>
  <div class="faq-list">
    <details><summary>Does the CLI include the sandbox service?</summary><p>The full release archive includes the service and verifier. The one-file CLI is the consumer interface and connects to a separately operated Rookhold endpoint.</p></details>
    <details><summary>Does MCP replace Claude Code or OpenCode?</summary><p>No. Your existing host owns the conversation and model. Rookhold adds four execution tools with policy and evidence.</p></details>
    <details><summary>Does adding Rookhold disable the host's shell?</summary><p>No. Remove or deny alternate shell and code-execution tools when the model must cross only the Rookhold boundary.</p></details>
    <details><summary>Can I safely run untrusted code on my laptop?</summary><p>No. The local demo is explicitly unisolated. Use the dedicated Linux gVisor deployment for the production boundary.</p></details>
  </div>
</section>

<section class="final-cta" aria-label="Download Rookhold">
  <h2>Give your agent a controlled place to run code.</h2>
  <a href="#download">Download Rookhold v0.7.1 <span aria-hidden="true">↓</span></a>
</section>
