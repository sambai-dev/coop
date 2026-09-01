import { defineConfig } from "vitepress";

const isVercel = process.env.VERCEL === "1";
const siteBase = isVercel ? "/" : "/rookhold/";
const siteOrigin = isVercel
  ? `https://${process.env.VERCEL_PROJECT_PRODUCTION_URL ?? "rookhold.vercel.app"}`
  : "https://sambai-dev.github.io/rookhold";

export default defineConfig({
  title: "Rookhold Sandbox",
  description: "A controlled execution boundary for AI agents, with hard limits, live output, and verifiable receipts.",
  lang: "en-US",
  base: siteBase,
  cleanUrls: true,
  lastUpdated: true,
  sitemap: { hostname: `${siteOrigin}/` },
  head: [
    ["meta", { name: "theme-color", content: "#12171e" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:title", content: "Rookhold Sandbox" }],
    ["meta", { property: "og:description", content: "Run agent code behind a boundary you control." }],
    ["meta", { property: "og:image", content: `${siteOrigin}/social-card.png` }],
    ["meta", { property: "og:url", content: `${siteOrigin}/` }],
    ["meta", { name: "twitter:card", content: "summary_large_image" }],
  ],
  themeConfig: {
    logo: "/rook.svg",
    search: { provider: "local" },
    nav: [
      { text: "Start", link: "/getting-started/quickstart" },
      { text: "Use", link: "/use/cli" },
      { text: "MCP", link: "/use/mcp" },
      { text: "Understand", link: "/understand/receipts" },
      { text: "Operate", link: "/deployment" },
      { text: "Download v0.7.1", link: "https://github.com/sambai-dev/rookhold/releases/tag/v0.7.1" },
    ],
    sidebar: {
      "/getting-started/": [
        {
          text: "Getting started",
          items: [
            { text: "Quickstart", link: "/getting-started/quickstart" },
            { text: "Installation", link: "/getting-started/installation" },
            { text: "First secure Linux deployment", link: "/getting-started/first-secure-deployment" },
          ],
        },
      ],
      "/use/": [
        {
          text: "Use Rookhold",
          items: [
            { text: "CLI", link: "/use/cli" },
            { text: "Python", link: "/use/python" },
            { text: "TypeScript", link: "/use/typescript" },
            { text: "MCP", link: "/use/mcp" },
            { text: "Recipes", link: "/use/recipes" },
          ],
        },
      ],
      "/understand/": [
        {
          text: "Understand Rookhold",
          items: [
            { text: "Execution model", link: "/understand/execution-model" },
            { text: "Limits", link: "/understand/limits" },
            { text: "Receipts", link: "/understand/receipts" },
            { text: "Isolation levels", link: "/understand/isolation" },
            { text: "Threat model", link: "/understand/threat-model" },
          ],
        },
      ],
      "/": [
        {
          text: "Operate Rookhold",
          items: [
            { text: "Configuration and deployment", link: "/deployment" },
            { text: "Operations", link: "/operations" },
            { text: "Upgrades", link: "/upgrading" },
            { text: "Observability", link: "/observability" },
            { text: "Troubleshooting", link: "/troubleshooting" },
            { text: "Compatibility", link: "/compatibility" },
            { text: "Projects using Rookhold", link: "/projects" },
          ],
        },
        {
          text: "Contribute",
          items: [
            { text: "Contribution tiers", link: "/contributing" },
            { text: "Runtime packs", link: "/runtime-packs" },
            { text: "Integrations", link: "/integrations" },
          ],
        },
      ],
    },
    socialLinks: [{ icon: "github", link: "https://github.com/sambai-dev/rookhold" }],
    editLink: { pattern: "https://github.com/sambai-dev/rookhold/edit/main/docs/:path" },
    footer: { message: "Short jobs. Hard limits. Receipts.", copyright: "Released under the MIT License." },
  },
});
