# MCP agent

Start from a configured Rookhold service, then run one of:

```bash
rookhold setup claude-code
rookhold setup hermes
rookhold setup opencode
rookhold setup generic-mcp
```

The command previews the change, backs up an existing configuration, writes no
secret value, and runs `rookhold check`. Disable the host's built-in shell or
code-execution tools when the model must use only Rookhold.
