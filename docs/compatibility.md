# Compatibility

| Surface | Supported |
|---|---|
| Strong server boundary | Linux x86_64 with the reviewed gVisor release |
| Development service | Linux, macOS, and Windows; trusted code only |
| Python package | Python 3.9–3.14 |
| TypeScript package | Node.js 18 and newer; modern browsers with Fetch |
| Job languages | Python, Node, and Bash when the service reports them available |
| MCP hosts checked on releases | Claude Code, Hermes, and OpenCode |
| Dashboard | Current stable Chrome, Edge, Firefox, and Safari |

## Compatibility policy before 1.0

- Public SDK members receive a documented deprecation period before removal.
- REST API breaking changes require a minor release and a migration note.
- Compatibility aliases state the release in which removal will be reconsidered.
- Release notes identify migrations and lead with why a change matters.
- Critical security fixes may override the normal deprecation period.

The internal `coop-*` crate names, v1 media types, evidence subject names, and
compatibility aliases remain because changing durable identities is riskier
than leaving internal history visible. New user-facing work uses Rookhold terms.
