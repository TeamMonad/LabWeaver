-- Control owns all membership facts. Access retains the identity directory.
-- This hard-cut catalog requires a new, explicitly authorized database baseline.
GRANT USAGE ON SCHEMA access TO lw_control_runtime;
-- Column reads are granted after 0004 creates the safe directory labels.
