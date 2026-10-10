BEGIN;

-- Migration 0039 returned marker-less queued/claimed outbounds to draft and
-- renamed their dispatch jobs to legacy_dispatch:<outbound>:<job>. The
-- outbound is provably unsent only when it remains draft, has no explicit
-- marker, and has no send_attempt. Release only its still-reserved reply
-- claim; submitted/accepted claim history is retained.
WITH reset_legacy_outbounds AS (
    SELECT o.workspace_id, o.id, o.mail_account_id, o.thread_id,
           o.reply_to_message_id
      FROM outbound_messages o
     WHERE o.status = 'draft'
       AND o.explicit_submission_at IS NULL
       AND o.reply_to_message_id IS NOT NULL
       AND NOT EXISTS (
            SELECT 1 FROM send_attempts sa
             WHERE sa.workspace_id = o.workspace_id
               AND sa.outbound_message_id = o.id
       )
       AND EXISTS (
            SELECT 1 FROM jobs j
             WHERE j.workspace_id = o.workspace_id
               AND j.kind = 'sending.dispatch_outbound'
               AND split_part(j.dedupe_key, ':', 1) = 'legacy_dispatch'
               AND j.payload->>'outbound_message_id' = o.id::text
       )
)
UPDATE reply_claims rc
   SET state = 'released'
  FROM reset_legacy_outbounds o
 WHERE rc.workspace_id = o.workspace_id
   AND rc.mail_account_id = o.mail_account_id
   AND rc.outbound_message_id = o.id
   AND rc.thread_id = o.thread_id
   AND rc.inbound_message_id = o.reply_to_message_id
   AND rc.state = 'reserved';

COMMIT;
