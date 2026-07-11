# Coverage Matrix

| Path | Positive | Negative | Current evidence | Required next evidence |
| --- | --- | --- | --- | --- |
| Service process | listener starts with explicit address | missing/invalid address fails | E1 unit/build | E2 process integration |
| Health contract | live/ready return versioned response | unknown route returns stable diagnostic | E1 unit | E2 HTTP integration for all services |
| Request correlation | response contains request ID | malformed external ID policy not yet frozen | E1 unit | Contract decision and negative tests |
| Telemetry | JSON subscriber initializes | invalid filter fails | compile and code review | captured structured-log tests |
| GitHub governance | Milestones/Labels/Issues/branch rules read back | insufficient Project scope is explicit | E0 metadata | Project field and Ready-state read-back |
| KubeVirt/Access/Evaluation | none | none | planned/blocked | E3/E4 evidence required by release gates |

