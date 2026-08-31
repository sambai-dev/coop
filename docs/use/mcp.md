# MCP

Configure one of the maintained hosts:

```bash
rookhold setup claude-code
rookhold setup hermes
rookhold setup opencode
```

The command finds the normal configuration file, shows a diff, creates a
timestamped backup, writes only after confirmation, keeps the API key in the
environment, and runs `rookhold check` afterward.

Adding Rookhold does not disable a host's built-in shell. Remove or deny other
execution tools when a model must cross only the Rookhold boundary.
