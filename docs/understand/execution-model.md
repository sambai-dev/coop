# Execution model

An authenticated caller submits one short, stateless job. The service clamps
the request to server policy, admits it through tenant and global capacity,
runs one process boundary, persists ordered events, and finalizes a receipt.

Rookhold is not a repository workspace, browser, remote desktop, package
installer, service host, or persistent computer. Inputs and requested outputs
are bounded per job and disappear from the executor after completion.
