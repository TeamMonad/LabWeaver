-- Control owns all membership facts. Access retains the identity directory.
-- This hard-cut catalog requires a new, explicitly authorized database baseline.
GRANT USAGE ON SCHEMA access TO lw_control_runtime;
GRANT SELECT ON access.actors TO lw_control_runtime;
