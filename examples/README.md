# Rookhold recipes

Each folder demonstrates one small application pattern against a configured
Rookhold service. Install `rookhold`, set `ROOKHOLD_BASE_URL` and
`ROOKHOLD_API_KEY`, then run the file shown in that folder.

| Recipe | Outcome |
|---|---|
| [python-basic](python-basic/hello.py) | Run Python in one method call. |
| [typescript-basic](typescript-basic/hello.mjs) | Run Python from Node.js. |
| [llm-tool-call](llm-tool-call/run-generated.py) | Bound a generated function. |
| [json-transform](json-transform/transform.py) | Send JSON on stdin and parse JSON output. |
| [evaluator](evaluator/evaluate.py) | Check candidate code against hidden cases. |
| [stdin-json](stdin-json/process.py) | Process structured application input. |
| [timeout-and-cancel](timeout-and-cancel/timeout.py) | Observe a hard wall-time outcome. |
| [receipt-verification](receipt-verification/save-receipt.py) | Save the execution receipt for offline verification. |
| [mcp-agent](mcp-agent/README.md) | Connect a supported MCP host. |

The local development service reports `isolation: none` and must only receive
trusted code. Use a Linux gVisor service for generated or user-submitted code.
