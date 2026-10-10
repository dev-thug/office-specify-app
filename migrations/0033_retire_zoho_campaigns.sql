-- Retire native Campaigns without deleting delivery/audit history or changing
-- Office SMTP, Zoho Mail, local suppression, or booking data.
-- Upgrade with the old API/workers stopped. No remote cancellation is claimed.

UPDATE delivery_channels
   SET enabled=false, policy_status='blocked', version=version+1
 WHERE provider='zoho_campaigns';

UPDATE integration_connections
   SET status='disabled', last_error_code='PROVIDER_RETIRED',
       secret_ref='retired:zoho_campaigns:' || id::text, version=version+1
 WHERE provider='zoho_campaigns';

UPDATE marketing_oauth_states
   SET used_at=coalesce(used_at,now()), expires_at=least(expires_at,now());
UPDATE integration_oauth_requests r
   SET used_at=coalesce(r.used_at,now()), expires_at=least(r.expires_at,now())
  FROM integration_connections c
 WHERE c.workspace_id=r.workspace_id AND c.id=r.connection_id
   AND c.provider='zoho_campaigns';

-- A claimed/running external action may already have reached the provider.
-- Its result remains unknown, never falsely completed or retried.
UPDATE jobs j
   SET state=CASE WHEN j.state='running' THEN 'unknown' ELSE 'canceled' END,
       last_error_code='PROVIDER_RETIRED', finished_at=now(),
       lease_owner=NULL, lease_token=NULL, lease_expires_at=NULL
 WHERE j.state IN ('queued','retry_wait','running') AND (
   j.kind IN ('marketing.poll_export','marketing.push_suppression','marketing.backfill_suppressions')
   OR (j.kind IN ('campaigns.freeze_launch','campaigns.activate_launch') AND EXISTS (
     SELECT 1 FROM campaign_launches l JOIN delivery_channels c
       ON c.workspace_id=l.workspace_id AND c.id=l.delivery_channel_id
      WHERE l.workspace_id=j.workspace_id AND l.id::text=j.payload->>'campaign_launch_id'
        AND c.provider='zoho_campaigns'))
   OR (j.kind='campaigns.schedule_next_step' AND EXISTS (
     SELECT 1 FROM campaign_enrollments e JOIN campaign_launches l
       ON l.workspace_id=e.workspace_id AND l.id=e.launch_id
       JOIN delivery_channels c ON c.workspace_id=l.workspace_id AND c.id=l.delivery_channel_id
      WHERE e.workspace_id=j.workspace_id AND e.id::text=j.payload->>'campaign_enrollment_id'
        AND c.provider='zoho_campaigns'))
   OR (j.kind IN ('sending.dispatch_outbound','sending.reconcile_outbound') AND EXISTS (
     SELECT 1 FROM outbound_messages o JOIN delivery_channels c
       ON c.workspace_id=o.workspace_id AND c.id=o.delivery_channel_id
      WHERE o.workspace_id=j.workspace_id AND o.id::text=j.payload->>'outbound_message_id'
        AND c.provider='zoho_campaigns'))
 );

UPDATE approval_requests a
   SET status='revoked', version=version+1
 WHERE a.status='pending' AND (
   EXISTS (SELECT 1 FROM campaign_launches l JOIN delivery_channels c
     ON c.workspace_id=l.workspace_id AND c.id=l.delivery_channel_id
     WHERE l.workspace_id=a.workspace_id AND l.id=a.launch_id AND c.provider='zoho_campaigns')
   OR EXISTS (SELECT 1 FROM outbound_messages o JOIN delivery_channels c
     ON c.workspace_id=o.workspace_id AND c.id=o.delivery_channel_id
     WHERE o.workspace_id=a.workspace_id AND o.id=a.outbound_message_id AND c.provider='zoho_campaigns')
 );

UPDATE outbound_messages o
   SET status=CASE WHEN o.status='submitting' OR EXISTS (
         -- Native bulk sends leave recipient placeholders in draft/queued;
         -- their local status alone never proves that they were not sent.
         SELECT 1 FROM marketing_export_recipients r
           JOIN marketing_exports e ON e.workspace_id=r.workspace_id AND e.id=r.export_id
          WHERE r.workspace_id=o.workspace_id AND r.outbound_message_id=o.id
            AND (e.state IN ('submitting','in_progress','unknown','completed')
              OR EXISTS (SELECT 1 FROM marketing_operations m
                WHERE m.workspace_id=e.workspace_id AND m.export_id=e.id
                  AND m.kind='send_campaign' AND m.status IN ('started','unknown','completed')))
       ) THEN 'unknown' ELSE 'canceled' END,
       failure_code='PROVIDER_RETIRED', version=o.version+1
  FROM delivery_channels c
 WHERE c.workspace_id=o.workspace_id AND c.id=o.delivery_channel_id
   AND c.provider='zoho_campaigns' AND o.status IN ('draft','queued','claimed','submitting');

UPDATE campaign_enrollments e
   SET status='stopped', stop_reason='provider_retired', next_send_at=NULL
  FROM campaign_launches l JOIN delivery_channels c
    ON c.workspace_id=l.workspace_id AND c.id=l.delivery_channel_id
 WHERE e.workspace_id=l.workspace_id AND e.launch_id=l.id
   AND c.provider='zoho_campaigns' AND e.status IN ('pending','active');

-- Building launches have no frozen manifest and cannot legally transition to
-- canceled. Leave those immutable draft artifacts inert behind a disabled
-- channel; their freeze jobs were canceled above.
UPDATE campaign_launches l
   SET status='canceled', version=l.version+1
  FROM delivery_channels c
 WHERE c.workspace_id=l.workspace_id AND c.id=l.delivery_channel_id
   AND c.provider='zoho_campaigns'
   AND l.status IN ('frozen','awaiting_approval','approved','running','paused');

-- Do not cancel a logical Office campaign whose latest revision has moved to
-- SMTP, even if it has older native history.
UPDATE campaigns c
   SET status='canceled', version=version+1, updated_at=now()
 WHERE c.status NOT IN ('completed','canceled') AND EXISTS (
   SELECT 1 FROM campaign_revisions r JOIN delivery_channels dc
     ON dc.workspace_id=r.workspace_id AND dc.id=r.delivery_channel_id
    WHERE r.workspace_id=c.workspace_id AND r.campaign_id=c.id
      AND dc.provider='zoho_campaigns'
      AND r.revision_no=(SELECT max(r2.revision_no) FROM campaign_revisions r2
        WHERE r2.workspace_id=c.workspace_id AND r2.campaign_id=c.id)
 );

UPDATE marketing_operations
   SET status='unknown', error_code='PROVIDER_RETIRED', completed_at=now(), updated_at=now()
 WHERE status='started';
UPDATE marketing_suppression_operations
   SET status='unknown', error_code='PROVIDER_RETIRED', completed_at=now()
 WHERE status='started';
UPDATE zoho_email_template_operations SET state='unknown' WHERE state='intent';

UPDATE marketing_exports e
   SET state=CASE
         WHEN e.state='submitting' THEN 'unknown'
         WHEN e.state='syncing' AND EXISTS (SELECT 1 FROM marketing_operations o
           WHERE o.workspace_id=e.workspace_id AND o.export_id=e.id AND o.status='unknown') THEN 'unknown'
         WHEN e.state IN ('preparing','syncing','needs_review','ready') THEN 'canceled'
         ELSE e.state END,
       report=e.report || '{"provider_retired":true,"provider_cancel_not_requested":true}'::jsonb,
       version=e.version+1
  FROM integration_connections c
 WHERE c.workspace_id=e.workspace_id AND c.id=e.connection_id AND c.provider='zoho_campaigns'
   AND NOT (e.report @> '{"privacy_erased":true}'::jsonb)
   AND e.content_hash<>decode(repeat('00',32),'hex');

-- Historical tables remain for audit and authorized local privacy scrubbing.
-- The active runtimes no longer need to read/write native export state.
REVOKE ALL ON marketing_connections, marketing_oauth_states,
    marketing_launch_values, marketing_exports, marketing_export_recipients,
    marketing_operations, marketing_export_reviews,
    marketing_suppression_operations, zoho_email_template_operations
FROM api_runtime, worker_runtime;

-- Local erasure may verify retirement without seeing OAuth secret references.
GRANT SELECT (workspace_id,id,provider,status)
    ON integration_connections TO retention_operator;
GRANT SELECT (workspace_id,id,provider,enabled,policy_status)
    ON delivery_channels TO retention_operator;

ALTER TABLE integration_connections ADD CONSTRAINT retired_campaigns_connection_disabled
    CHECK (provider<>'zoho_campaigns' OR status='disabled');
ALTER TABLE delivery_channels ADD CONSTRAINT retired_campaigns_channel_disabled
    CHECK (provider<>'zoho_campaigns' OR (NOT enabled AND policy_status='blocked'));
ALTER TABLE jobs ADD CONSTRAINT retired_campaigns_jobs_terminal
    CHECK (kind NOT IN ('marketing.poll_export','marketing.push_suppression','marketing.backfill_suppressions')
           OR state IN ('completed','dead','canceled','unknown'));
