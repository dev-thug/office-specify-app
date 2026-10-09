-- Maintenance must verify imported CSV references before deleting object bytes.
GRANT SELECT ON import_jobs TO retention_operator;
