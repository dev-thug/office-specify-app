import {useQuery} from '@tanstack/react-query';
import {apiFetch} from './client';
import {mutate} from './mutate';
import type {components} from './schema';
export type MarketingConnection = components['schemas']['MarketingConnectionResponse'];
export type CampaignsSetup = components['schemas']['CampaignsSetupResponse'];
export type MarketingList = components['schemas']['MarketingListResponse'];
export type NativeCampaign = components['schemas']['NativeCampaignResponse'];
export function useCampaignsSetup(enabled=true){return useQuery({queryKey:['marketing','setup'],enabled,queryFn:()=>apiFetch<CampaignsSetup>('/api/v1/integrations/zoho-campaigns')})}
export function connectCampaigns(key:string){return mutate<components["schemas"]["MarketingConnectResponse"]>('/api/v1/integrations/zoho-campaigns/connect',{method:'POST'},{idempotencyKey:key})}
export function getCampaignsLists(id:string){return apiFetch<MarketingList[]>('/api/v1/marketing/connections/'+encodeURIComponent(id)+'/lists')}
export function getRecentCampaigns(id:string){return apiFetch<NativeCampaign[]>('/api/v1/marketing/connections/'+encodeURIComponent(id)+'/campaigns')}
export function disconnectCampaigns(id:string,key:string){return mutate<components["schemas"]["MarketingDisconnectedResponse"]>('/api/v1/marketing/connections/'+encodeURIComponent(id)+'/disconnect',{method:'POST'},{idempotencyKey:key})}
