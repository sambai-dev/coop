# CLI

```bash
rookhold run python 'print(6 * 7)'
rookhold run python script.py --wall-seconds 2
rookhold run python transform.py --file data.csv --output output/report.json
rookhold check
rookhold jobs
rookhold show JOB_ID
rookhold verify --envelope job.dsse.json --subject job-result.json --public-key key.pem --tenant acme
```

`rookhold run` uses `ROOKHOLD_BASE_URL` and `ROOKHOLD_API_KEY` when both are
set. Otherwise it starts and stops a temporary local development service.
`rookhold check` tests the service, credential, runtimes, actual isolation, and
MCP connection with plain pass/warn/fail output.
