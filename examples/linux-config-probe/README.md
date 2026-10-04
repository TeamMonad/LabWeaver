# Linux configuration experiment

Use the complete `environment.yaml` and `evaluation.yaml` as the proposed
EnvironmentSpec and EvaluationSpec. Preserve their VM runtime, submission
collector, approved read-only playbook, assertions and deterministic score.
Publish the complete pair after normal teacher review. Do not replace the VM
with a container or score the submitted report with an LLM.

The current approved base-disk binding uses the provider's `lab` guest account
and `/home/lab/workspace` collector root. Verify these against the actual
provider configuration before using this package on another deployment. The
guest must have Python 3.9 or later, python3-apt, openssh-server and the active
`ssh` service. A missing prerequisite is a failure to address before running
the experiment; do not invent package or service facts.

Register your SSH public key on the normal student SSH-key page, start the
published lab, and use the SSH command and gateway fingerprint displayed on
the environment page. The managed guest accepts the platform's SSH certificate
through that route. No guest password or console autologin is needed.

Create `$HOME/workspace/application.conf` with the exact bytes in
`materials/application.conf` and permission mode `0644`. Create
`$HOME/workspace/report.md` describing the change. The workspace must match the
provider's configured collector root. Only `report.md` is frozen for submission;
the evaluator observes the configuration in the running VM using the approved
read-only `profiles/linux-config-probe-v1/playbook.yml`.

The evaluator first checks actual reachability, installed openssh-server and
active SSH service. It then awards 100 points only when the configuration
exists, its SHA-256 matches the approved material and its mode is `0644`.
Different content or permissions earn zero. Editing the report cannot change
these deterministic configuration checks. Each submission identifies its own
frozen report and running environment; a previous submission's facts are not
reused for a later observation.
