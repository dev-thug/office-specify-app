BEGIN;

-- Recipient snapshots may be replaced only while an export is preparing and
-- before any provider operation exists. The recipient trigger enforces that
-- boundary independently of this narrow runtime DELETE privilege.
GRANT DELETE ON marketing_export_recipients TO api_runtime, worker_runtime;

COMMIT;
