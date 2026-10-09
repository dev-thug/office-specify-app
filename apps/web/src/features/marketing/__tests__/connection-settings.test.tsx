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
  expect(screen.getByRole('button',{name:'Connect Zoho Campaigns'})).toBeDisabled();
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
});
