# CTF web lab: Flag Vault

## Goal

The Flag Vault web service exposes a login form and a proof submission form.
The service stores an operations account whose signed note contains the lab
flag. Your goal is to read that note without legitimate credentials and submit
the flag for scoring.

## Environment

- The Flag Vault service is reachable over the environment's HTTP entry at
  `/`. The same origin serves `/login`, `/submit`, and `/healthz`.
- The container has no interactive terminal. Interact with the service only
  through HTTP.
- Network egress is restricted. There is no external target and no internet
  access.

## Contract

- The flag has the form `FLAG{...}` and is stored in the service database.
- Submit your captured flag with `POST /submit` field `flag`. The service writes
  the value to `student/flag.txt` in the workspace.
- The platform collector snapshots `student/flag.txt`. The flag must match the
  hidden expected value exactly to score 100.
- A submission is valid only after the proof file exists and the captured flag
  matches. There is no partial credit.

## Hints

- The login endpoint reflects the outcome of a backend account lookup. Examine
  how the username and password you send are interpreted by that lookup.
- The guest account is not the target. The operations account holds the flag.
