#!/usr/bin/env python3
"""Seed fake-only browser fixtures; refuses every database except its QA DB."""
import json
import os
import subprocess
from pathlib import Path

sql = r'''
BEGIN;
DO $$ BEGIN
 IF current_database() <> 'outreach_codex_e2e' THEN RAISE EXCEPTION 'QA database required'; END IF;
END $$;
CREATE TEMP TABLE qa_fixture (value JSONB);
DO $$
DECLARE ws UUID; actor UUID; conn UUID; mailbox UUID; sender UUID; channel UUID;
 company UUID; contact UUID; address UUID; suffix TEXT := substr(gen_random_uuid()::text,1,8);
BEGIN
 SELECT p.workspace_id,p.id INTO ws,actor FROM principals p JOIN users u ON p.user_id=u.id
  WHERE u.email='codex-e2e@example.test' AND p.kind='human' AND p.role='admin' AND p.status='active';
 IF actor IS NULL THEN RAISE EXCEPTION 'bootstrap QA admin first'; END IF;
 PERFORM set_config('app.workspace_id',ws::text,true);
 UPDATE workspaces SET emergency_stop=false,settings=settings ||
  '{"public_base_url":"http://127.0.0.1:13000","booking_privacy_notice_version":"qa-v1"}'::jsonb WHERE id=ws;
 INSERT INTO integration_connections(workspace_id,provider,region,secret_ref,status,connected_by)
  VALUES(ws,'zoho_mail','us','synthetic-qa-no-real-secret','active',actor) RETURNING id INTO conn;
 INSERT INTO mail_accounts(workspace_id,connection_id,provider_account_id,primary_address,name,status)
  VALUES(ws,conn,'qa-'||suffix,'qa-sender-'||suffix||'@example.test','Synthetic QA inbox '||suffix,'active') RETURNING id INTO mailbox;
 INSERT INTO sender_identities(workspace_id,mail_account_id,address,display_name,status,verified_at)
  VALUES(ws,mailbox,'qa-sender-'||suffix||'@example.test','Synthetic QA sender','verified',now()) RETURNING id INTO sender;
 INSERT INTO mail_account_grants(workspace_id,mail_account_id,principal_id,can_read,can_compose,can_send,can_approve,can_assign,granted_by)
  VALUES(ws,mailbox,actor,true,true,true,true,true,actor);
 INSERT INTO delivery_channels(workspace_id,mail_account_id,sender_identity_id,connection_id,provider,purpose,name,policy_status,enabled)
  VALUES(ws,mailbox,sender,conn,'zoho_mail','correspondence','Synthetic QA correspondence '||suffix,'allowed',true) RETURNING id INTO channel;
 INSERT INTO channel_reviews(workspace_id,delivery_channel_id,reviewed_by,result,purpose,contract_evidence_ref)
  VALUES(ws,channel,actor,'allowed','correspondence','synthetic-test-fixture-only');
 INSERT INTO companies(workspace_id,name,owner_principal_id)
  VALUES(ws,'Synthetic QA company '||suffix,actor) RETURNING id INTO company;
 INSERT INTO contacts(workspace_id,company_id,name,owner_principal_id,status)
  VALUES(ws,company,'Synthetic QA contact '||suffix,actor,'active') RETURNING id INTO contact;
 INSERT INTO email_addresses(workspace_id,address_original,normalized_key,kind,verification_status,verified_at)
  VALUES(ws,'qa-recipient-'||suffix||'@example.test','qa-recipient-'||suffix||'@example.test','person','verified',now()) RETURNING id INTO address;
 INSERT INTO contact_emails(workspace_id,contact_id,email_address_id,is_primary) VALUES(ws,contact,address,true);
 INSERT INTO communication_bases(workspace_id,email_address_id,purpose,jurisdiction,basis_type,status,evidence_ref,collected_at,reviewed_by,reviewed_at)
  VALUES(ws,address,'correspondence','KR','explicit_consent','valid','synthetic-test-fixture-only',now(),actor,now());
 INSERT INTO qa_fixture VALUES(jsonb_build_object('contact_id',contact,'email_address_id',address,
  'email','qa-recipient-'||suffix||'@example.test','mail_account_id',mailbox,
  'mailbox_name','Synthetic QA inbox '||suffix,'mailbox_address','qa-sender-'||suffix||'@example.test','channel_name','Synthetic QA correspondence '||suffix));
END $$;
SELECT value FROM qa_fixture;
COMMIT;
'''
result = subprocess.run(
    ['psql','postgres://hyunjoong@localhost:5432/outreach_codex_e2e','-qAt','-v','ON_ERROR_STOP=1'],
    input=sql, text=True, capture_output=True, check=True,
)
fixture = json.loads(result.stdout.strip().splitlines()[-1])
path = Path('/tmp/outreach-browser-fixture.json')
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, 'w') as file:
    json.dump(fixture, file)
print('Seeded fake-only browser QA fixture')
