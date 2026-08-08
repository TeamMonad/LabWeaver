# Current role

- Developer: A (`@2018wzh`)
- Scope: architecture, Control/Access/Resource, public contracts, migrations, release-gate decisions, and integration ownership for Issues #124 and #142. For Issue #126, A owns candidate freeze, execution-ledger acquisition and connected execution after every recorded blocker is cleared; this does not transfer verification authority from D.
- Review boundary: B (`@zeyi2`) must review Agent/Environment/Evaluation, console security and Release Gate changes; D (`@Nova-Lciop-J`) independently verifies deployment, private browser artifacts and cluster readback for #126. C (`@yingxvemiao`) reviews frontend changes.
- Approval boundary: the author does not self-approve, merge, or declare the high-risk PR complete. Human review, connected verification, and release evidence remain separate gates.
- Data boundary: private credentials and deployment inputs stay in ignored/private or root-owned remote locations; repository evidence records only locators, hashes, counts, and diagnostics.
