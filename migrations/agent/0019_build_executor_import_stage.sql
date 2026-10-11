-- Keep the executor ledger aligned with the current supply-chain stages.
ALTER TABLE agent.build_executor_fences
    DROP CONSTRAINT build_executor_fences_last_stage_check,
    ADD CONSTRAINT build_executor_fences_last_stage_check CHECK (
        last_stage IN ('ensure_private_project','build','import','publish','cleanup')
    );
