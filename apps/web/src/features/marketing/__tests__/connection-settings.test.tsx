import { QueryClient,QueryClientProvider } from '@tanstack/react-query';
import { NextIntlClientProvider } from 'next-intl';
import { render,screen,waitFor,fireEvent } from '@testing-library/react';
import { afterEach,describe,it,expect,vi } from 'vitest';
import { CampaignsConnectionSettings } from '../connection-settings';
import settings from '../../../../messages/en/settings.json';
import common from '../../../../messages/en/common.json';
vi.mock('@/features/settings/use-me',()=>({useIsAdmin:()=>true}));
afterEach(()=>vi.unstubAllGlobals());
const setup={configured:false,missing_fields:['ZOHO_CAMPAIGNS_CLIENT_ID','ZOHO_CAMPAIGNS_CLIENT_SECRET'],callback_uri:'http://127.0.0.1:13000/api/v1/integrations/zoho-campaigns/callback',accounts_host:'https://accounts.zoho.com',campaigns_host:'https://campaigns.zoho.com',plan:'free',scheduled_send:false,webhooks:false,mail_oauth_configured:false,connections:[]};
function mount(){return render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}><NextIntlClientProvider locale="en" messages={{settings,common}}><CampaignsConnectionSettings /></NextIntlClientProvider></QueryClientProvider>)}
describe('Campaigns OAuth setup',()=>{
 it('explains missing client configuration instead of opening an invalid authorization URL',async()=>{
  vi.stubGlobal('fetch',vi.fn(async()=>new Response(JSON.stringify({data:setup}),{status:200,headers:{'content-type':'application/json'}})));
  mount();
  await screen.findByText('ZOHO_CAMPAIGNS_CLIENT_ID');
  expect(screen.getByText('ZOHO_CAMPAIGNS_CLIENT_SECRET')).toBeVisible();
  expect(screen.getByRole('button',{name:'Connect or reconnect Zoho Campaigns'})).toBeDisabled();
  expect(screen.getByDisplayValue(setup.callback_uri)).toBeVisible();
  expect(screen.queryByRole('link',{name:'Open authorization page'})).toBeNull();
 });
 it('verifies the real connected account by listing mailing lists',async()=>{
  const fetcher=vi.fn<typeof fetch>(async(input)=>new Response(JSON.stringify({data:String(input).includes('/lists')?[{key:'list-1',name:'Product news',contacts:3}]:{...setup,configured:true,missing_fields:[],connections:[{id:'connection-1',name:'Zoho Campaigns',status:'active',plan:'free',region:'com',lists_count:1,last_verified_at:null,created_at:'2026-10-09T00:00:00Z'}]}}),{status:200,headers:{'content-type':'application/json'}}));
  vi.stubGlobal('fetch',fetcher);mount();
  fireEvent.click(await screen.findByRole('button',{name:'Check mailing lists'}));
  await screen.findByText('Product news');
  await waitFor(()=>expect(fetcher.mock.calls.some(([url])=>String(url).includes('/marketing/connections/connection-1/lists'))).toBe(true));
 });
 it('reads recent native campaign names and statuses without offering create or send',async()=>{
  const connection={id:'connection-1',name:'Zoho Campaigns',status:'active',plan:'free',region:'com',lists_count:1,last_verified_at:null,created_at:'2026-10-09T00:00:00Z'};
  const fetcher=vi.fn<typeof fetch>(async(input)=>{
   const path=String(input);
   const data=path.includes('/marketing/connections/connection-1/campaigns')
    ?[{key:'campaign-1',name:'Summer newsletter',status:'Draft'}]
    :path.includes('/marketing/connections/connection-1/lists')
     ?[]
     :{...setup,configured:true,missing_fields:[],connections:[connection]};
   return new Response(JSON.stringify({data}),{status:200,headers:{'content-type':'application/json'}});
  });
  vi.stubGlobal('fetch',fetcher);mount();
  const recentButton=await screen.findByRole('button',{name:'View recent campaigns'});
  const consoleLink=screen.getByRole('link',{name:'Open Zoho Campaigns'});
  expect(consoleLink).toHaveAttribute('href','https://campaigns.zoho.com');
  expect(consoleLink).toHaveAttribute('target','_blank');
  expect(consoleLink).toHaveAttribute('rel','noopener noreferrer');
  expect(screen.queryByRole('button',{name:/create campaign/i})).toBeNull();
  expect(screen.queryByRole('button',{name:/send campaign/i})).toBeNull();
  fireEvent.click(recentButton);
  expect(await screen.findByText('Summer newsletter')).toBeInTheDocument();
  expect(screen.getByRole('dialog')).toBeInTheDocument();
  expect(screen.getByText('Draft')).toBeInTheDocument();
  expect(fetcher.mock.calls.some(([url])=>String(url)==='/api/v1/marketing/connections/connection-1/campaigns')).toBe(true);
 });
 it('asks active read-only tokens to reconnect for draft write scope',async()=>{
  const connected={id:'connection-1',name:'Zoho Campaigns',status:'active',plan:'free',region:'com',lists_count:0,last_verified_at:null,created_at:'2026-10-09T00:00:00Z'};
  const fetcher=vi.fn<typeof fetch>(async(input)=>{
   const path=new URL(String(input),'http://office.test').pathname;
   const data=path==='/api/v1/integrations/zoho-campaigns'?{...setup,configured:true,missing_fields:[],connections:[connected]}:
    path==='/api/v1/marketing/connections/connection-1/connect'?{authorization_url:'https://accounts.zoho.com/oauth'}:
    path==='/api/v1/mail-accounts'||path==='/api/v1/delivery-channels'?[]:
    path==='/api/v1/integrations/zoho-campaigns/connect'?{authorization_url:'https://accounts.zoho.com/oauth'}:
    {...setup,configured:true,missing_fields:[],connections:[connected]};
   return new Response(JSON.stringify({data,page:{next_cursor:null,has_more:false}}),{status:200,headers:{'content-type':'application/json'}});
  });
  vi.stubGlobal('fetch',fetcher);mount();
  expect(await screen.findByText('Some existing connections need updated access')).toBeVisible();
  fireEvent.click(screen.getByRole('button',{name:'Reconnect for write access'}));
  expect(await screen.findByRole('link',{name:'Open authorization page'})).toHaveAttribute('href','https://accounts.zoho.com/oauth');
  expect(fetcher.mock.calls.some(([url,init])=>String(url)==='/api/v1/integrations/zoho-campaigns/connect'&&init?.method==='POST')).toBe(true);
  const reconnectCall=fetcher.mock.calls.find(([url,init])=>String(url)==='/api/v1/integrations/zoho-campaigns/connect'&&init?.method==='POST');
  expect(new Headers(reconnectCall?.[1]?.headers).get('Idempotency-Key')).toBeTruthy();
 });
 it('registers evidence-backed senders and requires a separate review before enabling a Campaigns channel',async()=>{
  const connected={id:'connection-1',name:'Zoho Campaigns',status:'active',plan:'free',region:'com',lists_count:0,last_verified_at:null,created_at:'2026-10-09T00:00:00Z'};
  const account={id:'mail-1',connection_id:'mail-connection-1',provider:'zoho_mail',provider_account_id:'zoho-mail-1',primary_address:'team@example.test',name:'Team inbox',status:'active',last_synced_at:null,version:1,created_at:'2026-10-09T00:00:00Z'};
  let senderRows=[{id:'sender-pending',address:'pending@example.test',display_name:'Pending',status:'pending'}];
  let channel:{id:string;mail_account_id:string;sender_identity_id:string;connection_id:string;provider:string;purpose:string;name:string;policy_status:string;enabled:boolean;version:number}|null=null;
  const calls:{path:string;method:string;body:Record<string,unknown>|null;headers:Headers}[]=[];
  const fetcher=vi.fn<typeof fetch>(async(input,init)=>{
   const url=new URL(String(input),'http://office.test');const path=url.pathname;const method=init?.method??'GET';
   const headers=new Headers(init?.headers);const body=method==='POST'||method==='PATCH'?JSON.parse(String(init?.body??'{}')) as Record<string,unknown>:null;
   calls.push({path,method,body,headers});
   let data:unknown;
   let status=200;
   if(path==='/api/v1/integrations/zoho-campaigns')data={...setup,configured:true,missing_fields:[],connections:[connected]};
   else if(path==='/api/v1/mail-accounts')data=[account];
   else if(path==='/api/v1/marketing/sender-identities'&&method==='GET')data=senderRows;
   else if(path==='/api/v1/marketing/sender-identities'&&method==='POST'){
    const created={id:'sender-verified',address:String(body?.address),display_name:String(body?.display_name),status:'verified'};senderRows=[...senderRows,created];data=created;status=201;
   }
   else if(path==='/api/v1/delivery-channels'&&method==='GET')data=channel?[channel]:[];
   else if(path==='/api/v1/delivery-channels'&&method==='POST'){
    channel={id:'channel-1',mail_account_id:String(body?.mail_account_id),sender_identity_id:String(body?.sender_identity_id),connection_id:String(body?.connection_id),provider:'zoho_campaigns',purpose:'marketing',name:String(body?.name),policy_status:'unverified',enabled:false,version:1};data=channel;status=201;
   }
   else if(path==='/api/v1/delivery-channels/channel-1/reviews'&&method==='POST'){
    if(body&&'purpose' in body)return new Response(JSON.stringify({code:'VALIDATION_FAILED',detail:'unknown field purpose'}),{status:422,headers:{'content-type':'application/problem+json'}});
    if(channel)channel={...channel,policy_status:'allowed',version:2};data={id:'review-1',delivery_channel_id:'channel-1',reviewed_by:'admin-1',result:'allowed',purpose:'marketing',policy_url:body?.policy_url,contract_evidence_ref:body?.contract_evidence_ref,notes:body?.notes,reviewed_at:'2026-10-10T00:00:00Z',created_at:'2026-10-10T00:00:00Z'};status=201;
   }
   else if(path==='/api/v1/delivery-channels/channel-1'&&method==='PATCH'){
    if(channel)channel={...channel,enabled:true,version:3};data=channel;
   }
   else data=[];
   return new Response(JSON.stringify({data,page:{next_cursor:null,has_more:false}}),{status,headers:{'content-type':'application/json'}});
  });
  vi.stubGlobal('fetch',fetcher);mount();
  const address='newsletter@example.test';
  await screen.findByText('No verified sender is registered for this account yet.');
  fireEvent.change(screen.getByLabelText('Verified sender email'),{target:{value:address}});
  fireEvent.change(screen.getByLabelText('Sender display name'),{target:{value:'Office News'}});
  fireEvent.change(screen.getByLabelText('Zoho verification evidence URL'),{target:{value:'https://campaigns.zoho.com/settings/sender'}});
  fireEvent.click(screen.getByLabelText('I have already verified this sender in Zoho Campaigns and the evidence URL supports that claim.'));
  fireEvent.click(screen.getByRole('button',{name:'Register sender evidence'}));
  await screen.findAllByText('Office News <newsletter@example.test>');
  await waitFor(()=>expect(calls.some(call=>call.path==='/api/v1/marketing/sender-identities'&&call.method==='POST')).toBe(true));
  const registered=calls.find(call=>call.path==='/api/v1/marketing/sender-identities'&&call.method==='POST')!;
  expect(registered.body).toEqual({mail_account_id:'mail-1',address,display_name:'Office News',verification_evidence_ref:'https://campaigns.zoho.com/settings/sender'});
  expect(registered.headers.get('Idempotency-Key')).toBeTruthy();

  fireEvent.change(screen.getByLabelText('Channel name'),{target:{value:'Office Campaigns marketing'}});
  const createChannel=screen.getByRole('button',{name:'Create disabled channel'});
  await waitFor(()=>expect(createChannel).toBeEnabled());
  fireEvent.click(createChannel);
  await screen.findByRole('button',{name:'Review and add evidence'});
  const channelCreate=calls.find(call=>call.path==='/api/v1/delivery-channels'&&call.method==='POST')!;
  expect(channelCreate.body).toEqual({connection_id:'connection-1',mail_account_id:'mail-1',sender_identity_id:'sender-verified',purpose:'marketing',name:'Office Campaigns marketing'});
  expect(channelCreate.body).not.toHaveProperty('provider');
  expect(channelCreate.headers.get('Idempotency-Key')).toBeTruthy();

  fireEvent.click(screen.getByRole('button',{name:'Review and add evidence'}));
  fireEvent.change(screen.getByLabelText('Provider policy URL'),{target:{value:'https://www.zoho.com/campaigns/terms.html'}});
  fireEvent.change(screen.getByLabelText('Contract or policy evidence reference'),{target:{value:'https://docs.example.test/marketing-approval'}});
  fireEvent.click(screen.getByLabelText('I reviewed this evidence and confirmed that this verified sender may be used for marketing.'));
  fireEvent.click(screen.getByRole('button',{name:'Save allowed review'}));
  await waitFor(()=>expect(calls.some(call=>call.path==='/api/v1/delivery-channels/channel-1/reviews'&&call.method==='POST')).toBe(true));
  await screen.findByText(/Policy: allowed · disabled/);
  const reviewCall=calls.find(call=>call.path==='/api/v1/delivery-channels/channel-1/reviews')!;
  expect(reviewCall.body).not.toHaveProperty('purpose');
  expect(reviewCall.body).toMatchObject({result:'allowed',policy_url:'https://www.zoho.com/campaigns/terms.html',contract_evidence_ref:'https://docs.example.test/marketing-approval'});
  expect(reviewCall.headers.get('Idempotency-Key')).toBeTruthy();

  fireEvent.click(screen.getByRole('button',{name:'Enable channel'}));
  await screen.findByText(/Policy: allowed · enabled/);
  const enableCall=calls.find(call=>call.path==='/api/v1/delivery-channels/channel-1'&&call.method==='PATCH')!;
  expect(enableCall.body).toEqual({enabled:true});
  expect(enableCall.headers.get('If-Match')).toBe('"2"');
  expect(enableCall.headers.get('Idempotency-Key')).toBeTruthy();
 });
});
