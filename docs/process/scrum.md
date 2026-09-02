# GitHub Scrum Process

The organization Project is `LabWeaver Delivery`; the repository is `TeamMonad/LabWeaver`. GitHub exposes the built-in `Status`, `Priority`, and `Target date` as Issue-derived fields. The writable Scrum projections are `Workflow Status` and `Delivery Priority`; target dates are written through the Issue field API.

## Flow

```text
Backlog -> Ready -> In Progress -> Draft PR -> In Review -> Verify -> Done
```

Any state may transition to Blocked. A Blocked item records the original diagnostic, unblock owner and exit condition. Each member may hold one implementation Issue and one Review simultaneously.

## Evidence rule

Project metadata, Issue text, PR checks and implementation status serve different purposes. An Issue body saying `Status: Ready` does not prove that `Workflow Status` was updated. Missing GitHub scopes or unsupported repository controls remain explicit blockers.
