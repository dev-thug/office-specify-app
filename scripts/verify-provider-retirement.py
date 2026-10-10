#!/usr/bin/env python3
"""Verify a real pre-0033 upgrade in a disposable PostgreSQL database.

Requires RETIREMENT_TEST_DATABASE_URL pointing at an operator-owned isolated
outreach_provider_retirement database and PostgreSQL CLI/CREATEDB privileges.
Never modifies or drops that base database, or the live Office database.
"""
import json
import os
from pathlib import Path
import subprocess
from urllib.parse import unquote, urlsplit, urlunsplit
import uuid

ROOT = Path(__file__).resolve().parents[1]


def sql(url, text):
    parsed = urlsplit(url)
    env = dict(os.environ, PGDATABASE=unquote(parsed.path.lstrip("/")))
    if parsed.hostname:
        env["PGHOST"] = parsed.hostname
    if parsed.port:
        env["PGPORT"] = str(parsed.port)
    if parsed.username:
        env["PGUSER"] = unquote(parsed.username)
    if parsed.password:
        env["PGPASSWORD"] = unquote(parsed.password)
    result = subprocess.run(
        ["psql", "--no-psqlrc", "--quiet", "--tuples-only", "--no-align", "--set", "ON_ERROR_STOP=1"],
        input=text, text=True, capture_output=True, env=env, check=False,
    )
    if result.returncode:
        raise RuntimeError(result.stderr)
    return result.stdout.strip()


def main():
    base = os.environ["RETIREMENT_TEST_DATABASE_URL"]
    parsed = urlsplit(base)
    assert parsed.path == "/outreach_provider_retirement", "Use the isolated retirement test DB"
    name = "outreach_retire_test_" + uuid.uuid4().hex
    url = urlunsplit(parsed._replace(path="/" + name))
    sql(base, f'CREATE DATABASE "{name}";')
    try:
        for path in sorted((ROOT / "migrations").glob("*.sql")):
            if int(path.name.split("_", 1)[0]) < 33:
                sql(url, path.read_text())
        sql(url, (ROOT / "scripts/fixtures/legacy-campaigns.sql").read_text())
        before = json.loads(sql(url, snapshot()))
        assert before["native_active"] == 2 and before["smtp_active"] == 2, before
        sql(url, (ROOT / "migrations/0033_retire_zoho_campaigns.sql").read_text())
        after = json.loads(sql(url, snapshot()))
        expected = {
            "native_active": 0, "native_disabled": 2, "native_enabled": 0,
            "smtp_active": 2, "smtp_enabled": 2, "mail_active": 2,
            "native_queued_canceled": 2, "native_running_unknown": 2,
            "freeze_canceled": 2, "activation_canceled": 2,
            "dispatch_canceled": 2, "smtp_job_queued": 2,
            "approval_revoked": 2, "launch_canceled": 2, "building_preserved": 2,
            "native_outbound_canceled": 2, "inflight_outbound_unknown": 2,
            "smtp_outbound_queued": 2, "export_unknown": 2,
            "operation_unknown": 2, "history_bodies_preserved": 2,
            "mapped_outbound_unknown": 2, "enrollment_stopped": 2,
            "schedule_canceled": 2, "retirement_markers": 2,
            "api_native_read": 0, "worker_native_write": 0, "retention_scrub_grant": 1,
            "retention_status_read": 1, "retention_secret_read": 0,
        }
        assert after == expected, {"expected": expected, "actual": after}
        sql(url, """
DO $$ BEGIN
  BEGIN
    UPDATE integration_connections SET status='active' WHERE provider='zoho_campaigns';
    RAISE EXCEPTION 'retired connection reactivation unexpectedly allowed';
  EXCEPTION WHEN check_violation THEN NULL; END;
  BEGIN
    UPDATE jobs SET state='queued' WHERE kind='marketing.push_suppression';
    RAISE EXCEPTION 'retired jobs unexpectedly retryable';
  EXCEPTION WHEN check_violation THEN NULL; END;
  BEGIN
    UPDATE delivery_channels SET policy_status='allowed',enabled=true WHERE provider='zoho_campaigns';
    RAISE EXCEPTION 'retired channel reactivation unexpectedly allowed';
  EXCEPTION WHEN check_violation THEN NULL; END;
END $$;
""")
        print("Provider retirement upgrade passed: two workspaces, SMTP/Mail preserved, pending work retired, uncertain outcomes and history preserved")
    finally:
        sql(base, f'DROP DATABASE "{name}";')


def snapshot():
    def count(table, kind, condition):
        return f"(SELECT count(*) FROM {table} t JOIN _retirement_fixture f ON f.id=t.id WHERE f.kind='{kind}' AND {condition})"
    entries = {
        "native_active": count("integration_connections", "native_connection", "t.status='active'"),
        "native_disabled": count("integration_connections", "native_connection", "t.status='disabled'"),
        "native_enabled": count("delivery_channels", "native_channel", "t.enabled"),
        "smtp_active": count("smtp_connections", "smtp_connection", "t.status='active'"),
        "smtp_enabled": count("delivery_channels", "smtp_channel", "t.enabled AND t.policy_status='allowed'"),
        "mail_active": count("integration_connections", "mail_connection", "t.status='active'"),
        "native_queued_canceled": count("jobs", "native_queued_job", "t.state='canceled'"),
        "native_running_unknown": count("jobs", "native_running_job", "t.state='unknown' AND t.lease_token IS NULL"),
        "freeze_canceled": count("jobs", "freeze_job", "t.state='canceled'"),
        "activation_canceled": count("jobs", "activation_job", "t.state='canceled'"),
        "dispatch_canceled": count("jobs", "native_dispatch_job", "t.state='canceled'"),
        "smtp_job_queued": count("jobs", "smtp_job", "t.state='queued'"),
        "approval_revoked": count("approval_requests", "approval", "t.status='revoked'"),
        "launch_canceled": count("campaign_launches", "launch", "t.status='canceled'"),
        "building_preserved": count("campaign_launches", "building", "t.status='building'"),
        "native_outbound_canceled": count("outbound_messages", "native_outbound", "t.status='canceled'"),
        "inflight_outbound_unknown": count("outbound_messages", "inflight_outbound", "t.status='unknown'"),
        "smtp_outbound_queued": count("outbound_messages", "smtp_outbound", "t.status='queued'"),
        "export_unknown": count("marketing_exports", "export", "t.state='unknown'"),
        "operation_unknown": count("marketing_operations", "operation", "t.status='unknown' AND t.completed_at IS NOT NULL"),
        "history_bodies_preserved": count("marketing_exports", "export", "t.body_text='Historical native content'"),
        "mapped_outbound_unknown": count("outbound_messages", "mapped_outbound", "t.status='unknown'"),
        "enrollment_stopped": count("campaign_enrollments", "enrollment", "t.status='stopped' AND t.next_send_at IS NULL"),
        "schedule_canceled": count("jobs", "schedule_job", "t.state='canceled'"),
        "retirement_markers": count("marketing_exports", "export", "t.report @> '{\"provider_retired\":true,\"provider_cancel_not_requested\":true}'::jsonb"),
        "api_native_read": "CASE WHEN has_table_privilege('api_runtime','marketing_exports','SELECT') THEN 1 ELSE 0 END",
        "worker_native_write": "CASE WHEN has_table_privilege('worker_runtime','marketing_operations','INSERT') THEN 1 ELSE 0 END",
        "retention_scrub_grant": "CASE WHEN has_table_privilege('retention_operator','marketing_exports','UPDATE') THEN 1 ELSE 0 END",
        "retention_status_read": "CASE WHEN has_column_privilege('retention_operator','integration_connections','status','SELECT') THEN 1 ELSE 0 END",
        "retention_secret_read": "CASE WHEN has_column_privilege('retention_operator','integration_connections','secret_ref','SELECT') THEN 1 ELSE 0 END",
    }
    return "SELECT json_build_object(" + ",".join("'"+key+"',"+value for key,value in entries.items()) + ");"


if __name__ == "__main__":
    main()
