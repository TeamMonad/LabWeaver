# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | planned: listener starts with explicit address | planned: missing/invalid address fails | E0 in this commit; E1 validation is pending PR #21 | rerun E1 after merge, then E2 process integration |
| Health contract | planned: live/ready return versioned response | planned: unknown route returns stable diagnostic | E0 in this commit; E1 validation is pending PR #21 | rerun E1 after merge, then E2 HTTP integration for all services |
| Request correlation | planned: response contains request ID | malformed external ID policy not yet frozen | E0 in this commit; E1 validation is pending PR #21 | Contract decision and negative tests after merge |
| Telemetry | planned: JSON subscriber initializes | planned: invalid filter fails | E0 in this commit; implementation and validation are pending PR #21 | captured structured-log tests after merge |
| GitHub governance | Milestones/Labels/Issues/branch rules and 15 P0 Ready items read back | insufficient scope was diagnosed and resolved in Issue #20 | E0 metadata | human review and continued Project maintenance |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |
