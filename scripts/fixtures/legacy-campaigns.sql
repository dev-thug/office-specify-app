-- Synthetic pre-retirement rows only; used in disposable upgrade-test databases.
CREATE TABLE _retirement_fixture (kind text NOT NULL, id uuid PRIMARY KEY);
DO $$
DECLARE
  n integer; ws uuid; actor uuid; mail_conn uuid; mail uuid; sender uuid;
  native_conn uuid; native_channel uuid; smtp_conn uuid; smtp_channel uuid;
  tmpl uuid; tv uuid; campaign uuid; revision uuid; step uuid; launch uuid; building uuid;
  address uuid; outbound uuid; inflight uuid; smtp_outbound uuid; approval uuid;
  enrollment uuid; mapped_outbound uuid;
  export_id uuid; operation uuid; job uuid;
  hash bytea := decode(repeat('11',32),'hex');
BEGIN
FOR n IN 1..2 LOOP
  INSERT INTO workspaces(name) VALUES ('Retirement fixture '||n) RETURNING id INTO ws;
  INSERT INTO principals(workspace_id,kind,name,role) VALUES(ws,'system','Retirement fixture actor','system') RETURNING id INTO actor;
  INSERT INTO integration_connections(workspace_id,provider,region,secret_ref,status,connected_by)
    VALUES(ws,'zoho_mail','us','fixture-mail','active',actor) RETURNING id INTO mail_conn;
  INSERT INTO mail_accounts(workspace_id,connection_id,provider_account_id,primary_address,name,status)
    VALUES(ws,mail_conn,'fixture','sender@example.test','Fixture mailbox','active') RETURNING id INTO mail;
  INSERT INTO sender_identities(workspace_id,mail_account_id,address,display_name,status)
    VALUES(ws,mail,'sender@example.test','Fixture sender','verified') RETURNING id INTO sender;
  INSERT INTO integration_connections(workspace_id,provider,region,secret_ref,status,connected_by)
    VALUES(ws,'zoho_campaigns','us','fixture-native','active',actor) RETURNING id INTO native_conn;
  INSERT INTO marketing_connections(id,workspace_id) VALUES(native_conn,ws);
  INSERT INTO delivery_channels(workspace_id,mail_account_id,sender_identity_id,connection_id,provider,purpose,name,policy_status,enabled)
    VALUES(ws,mail,sender,native_conn,'zoho_campaigns','marketing','Old native channel','allowed',true) RETURNING id INTO native_channel;
  INSERT INTO integration_connections(workspace_id,provider,region,secret_ref,status,connected_by)
    VALUES(ws,'delivery_provider','smtp','fixture-smtp','active',actor) RETURNING id INTO smtp_conn;
  INSERT INTO delivery_channels(workspace_id,mail_account_id,sender_identity_id,connection_id,provider,purpose,name,policy_status,enabled,capabilities)
    VALUES(ws,mail,sender,smtp_conn,'delivery_provider','marketing','Office SMTP','allowed',true,'{"transport":"smtp"}') RETURNING id INTO smtp_channel;
  INSERT INTO smtp_connections(id,workspace_id,mail_account_id,sender_identity_id,channel_id,name,host,port,security,username,from_address,from_name,status)
    VALUES(smtp_conn,ws,mail,sender,smtp_channel,'SMTP fixture','smtp.example.test',465,'tls','sender@example.test','sender@example.test','Fixture','active');
  INSERT INTO email_templates(workspace_id,name,created_by) VALUES(ws,'Fixture template',actor) RETURNING id INTO tmpl;
  INSERT INTO template_versions(workspace_id,template_id,version_no,subject,body_text,variable_schema,content_hash,created_by)
    VALUES(ws,tmpl,1,'Fixture','Fixture','{}',hash,actor) RETURNING id INTO tv;
  INSERT INTO campaigns(workspace_id,mail_account_id,name,owner_principal_id)
    VALUES(ws,mail,'Old native campaign',actor) RETURNING id INTO campaign;
  INSERT INTO campaign_revisions(workspace_id,campaign_id,mail_account_id,delivery_channel_id,revision_no,timezone,schedule_rules,created_by)
    VALUES(ws,campaign,mail,native_channel,1,'UTC','{}',actor) RETURNING id INTO revision;
  INSERT INTO campaign_steps(workspace_id,revision_id,template_version_id,step_order)
    VALUES(ws,revision,tv,1) RETURNING id INTO step;
  INSERT INTO campaign_launches(workspace_id,revision_id,delivery_channel_id,requested_by,expires_at)
    VALUES(ws,revision,native_channel,actor,now()+interval '1 day') RETURNING id INTO launch;
  INSERT INTO campaign_launches(workspace_id,revision_id,delivery_channel_id,requested_by,expires_at)
    VALUES(ws,revision,native_channel,actor,now()+interval '1 day') RETURNING id INTO building;
  INSERT INTO approval_requests(workspace_id,requested_by,launch_id,action_type,snapshot_hash,expires_at)
    VALUES(ws,actor,launch,'campaign_launch',hash,now()+interval '1 day') RETURNING id INTO approval;
  INSERT INTO email_addresses(workspace_id,address_original,normalized_key)
    VALUES(ws,'recipient@example.test','recipient@example.test') RETURNING id INTO address;
  INSERT INTO campaign_enrollments(workspace_id,launch_id,revision_id,email_address_id,current_step_id,status)
    VALUES(ws,launch,revision,address,step,'active') RETURNING id INTO enrollment;
  INSERT INTO outbound_messages(workspace_id,mail_account_id,delivery_channel_id,sender_identity_id,to_email_address_id,requested_by,purpose,from_address_snapshot,to_address_snapshot,subject,body_text,payload_hash,status,launch_id,revision_id,enrollment_id,step_id)
    VALUES(ws,mail,native_channel,sender,address,actor,'marketing','sender@example.test','recipient@example.test','Possibly sent bulk placeholder','History',hash,'draft',launch,revision,enrollment,step) RETURNING id INTO mapped_outbound;
  UPDATE campaign_launches SET status='frozen',manifest_hash=hash,frozen_at=now() WHERE id=launch;
  INSERT INTO outbound_messages(workspace_id,mail_account_id,delivery_channel_id,sender_identity_id,to_email_address_id,requested_by,purpose,from_address_snapshot,to_address_snapshot,subject,body_text,payload_hash,status)
    VALUES(ws,mail,native_channel,sender,address,actor,'marketing','sender@example.test','recipient@example.test','Old native','History',hash,'queued') RETURNING id INTO outbound;
  INSERT INTO outbound_messages(workspace_id,mail_account_id,delivery_channel_id,sender_identity_id,to_email_address_id,requested_by,purpose,from_address_snapshot,to_address_snapshot,subject,body_text,payload_hash,status)
    VALUES(ws,mail,native_channel,sender,address,actor,'marketing','sender@example.test','recipient@example.test','Old inflight','History',hash,'submitting') RETURNING id INTO inflight;
  INSERT INTO outbound_messages(workspace_id,mail_account_id,delivery_channel_id,sender_identity_id,to_email_address_id,requested_by,purpose,from_address_snapshot,to_address_snapshot,subject,body_text,payload_hash,status)
    VALUES(ws,mail,smtp_channel,sender,address,actor,'marketing','sender@example.test','recipient@example.test','Keep SMTP','History',hash,'queued') RETURNING id INTO smtp_outbound;
  INSERT INTO _retirement_fixture VALUES
    ('native_connection',native_conn),('mail_connection',mail_conn),('smtp_connection',smtp_conn),
    ('native_channel',native_channel),('smtp_channel',smtp_channel),('launch',launch),('building',building),
    ('approval',approval),('native_outbound',outbound),('inflight_outbound',inflight),('smtp_outbound',smtp_outbound),
    ('enrollment',enrollment),('mapped_outbound',mapped_outbound);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key) VALUES(ws,'marketing.push_suppression','{}','queued') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('native_queued_job',job);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key,state,lease_owner,lease_token,lease_expires_at)
    VALUES(ws,'marketing.push_suppression','{}','running','running','fixture',gen_random_uuid(),now()+interval '1 hour') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('native_running_job',job);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key) VALUES(ws,'campaigns.freeze_launch',jsonb_build_object('campaign_launch_id',building),'freeze') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('freeze_job',job);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key) VALUES(ws,'campaigns.activate_launch',jsonb_build_object('campaign_launch_id',launch),'activate') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('activation_job',job);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key) VALUES(ws,'campaigns.schedule_next_step',jsonb_build_object('campaign_enrollment_id',enrollment),'schedule') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('schedule_job',job);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key) VALUES(ws,'sending.dispatch_outbound',jsonb_build_object('outbound_message_id',outbound),'native-dispatch') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('native_dispatch_job',job);
  INSERT INTO jobs(workspace_id,kind,payload,dedupe_key) VALUES(ws,'sending.dispatch_outbound',jsonb_build_object('outbound_message_id',smtp_outbound),'smtp-dispatch') RETURNING id INTO job;
  INSERT INTO _retirement_fixture VALUES('smtp_job',job);
  INSERT INTO marketing_exports(workspace_id,launch_id,revision_id,step_id,delivery_channel_id,connection_id,source_manifest_hash,content_hash,audience_hash,export_hash,subject,body_text,body_html,sender_address,sender_name,field_map,state,prepared_by,expires_at)
    VALUES(ws,launch,revision,step,native_channel,native_conn,hash,hash,hash,hash,'History','Historical native content','<p>History</p>','sender@example.test','Fixture','{}','preparing',actor,now()+interval '1 day') RETURNING id INTO export_id;
  INSERT INTO marketing_export_recipients(workspace_id,export_id,email_address_id,outbound_message_id,normalized_address,variables)
    VALUES(ws,export_id,address,mapped_outbound,'recipient@example.test','{}');
  UPDATE marketing_exports SET state='syncing' WHERE id=export_id;
  UPDATE marketing_exports SET state='ready' WHERE id=export_id;
  UPDATE marketing_exports SET state='submitting' WHERE id=export_id;
  INSERT INTO marketing_operations(workspace_id,export_id,kind,correlation_key,request_hash,status)
    VALUES(ws,export_id,'send_campaign','fixture',hash,'started') RETURNING id INTO operation;
  INSERT INTO _retirement_fixture VALUES('export',export_id),('operation',operation);
END LOOP;
END $$;
