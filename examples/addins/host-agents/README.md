# Agents across hosts

Copy this directory into `RIWORK_HOME/addins/host-agents`, then open and trust it
from Add-ins. Somtus generates `addin-api.ts` when it builds the example.

`hosts.read` grants metadata for all paired hosts: session titles, provider and
execution status, reported subagent counts, pending-request kinds and connection
freshness. It does not grant transcript access, approval responses or execution.
Session keys are `(host.id, session.kind, session.id)`; connection IDs select a
route and must not be used as execution identity. A stale working session remains
last-known working. Unsubscribing or revoking trust stops event delivery.
