"use client";
import {useState} from 'react';
import {useTranslations} from 'next-intl';
import {useMutation,useQueryClient} from '@tanstack/react-query';
import {ExternalLinkIcon,LinkIcon,RefreshCwIcon} from 'lucide-react';
import {Button} from '@/components/ui/button';
import {Badge} from '@/components/ui/badge';
import {Card,CardContent,CardHeader,CardTitle,CardDescription} from '@/components/ui/card';
import {Alert,AlertTitle,AlertDescription} from '@/components/ui/alert';
import {Dialog,DialogContent,DialogHeader,DialogTitle,DialogDescription,DialogFooter} from '@/components/ui/dialog';
import {ErrorState} from '@/components/patterns/error-state';
import {CopyField} from '@/features/settings/copy-field';
import {useIsAdmin} from '@/features/settings/use-me';
import {idempotencyKey} from '@/lib/api/client';
import {useCampaignsSetup,connectCampaigns,getCampaignsLists,getRecentCampaigns,disconnectCampaigns,type MarketingList,type MarketingConnection,type NativeCampaign} from '@/lib/api/marketing';

export function CampaignsConnectionSettings(){
 const t=useTranslations('settings.campaigns');const admin=useIsAdmin();const qc=useQueryClient();
 const setup=useCampaignsSetup(admin);const [authorization,setAuthorization]=useState<string|null>(null);
 const [lists,setLists]=useState<MarketingList[]|null>(null);const [disconnectFor,setDisconnectFor]=useState<MarketingConnection|null>(null);
 const [recentCampaignsFor,setRecentCampaignsFor]=useState<MarketingConnection|null>(null);
 const [recentCampaigns,setRecentCampaigns]=useState<NativeCampaign[]|null>(null);const [recentCampaignsError,setRecentCampaignsError]=useState(false);
 const [error,setError]=useState(false);
 const connect=useMutation({mutationFn:()=>connectCampaigns(idempotencyKey()),onSuccess:result=>{setAuthorization(result.data.authorization_url ?? null);setError(!result.data.authorization_url);void qc.invalidateQueries({queryKey:['marketing','setup']});},onError:()=>setError(true)});
 const verify=useMutation({mutationFn:(id:string)=>getCampaignsLists(id),onSuccess:result=>{setLists(result.data);setError(false);void qc.invalidateQueries({queryKey:['marketing','setup']});},onError:()=>setError(true)});
 const recent=useMutation({mutationFn:(id:string)=>getRecentCampaigns(id),onSuccess:result=>{setRecentCampaigns(result.data);setRecentCampaignsError(false);},onError:()=>setRecentCampaignsError(true)});
 const disconnect=useMutation({mutationFn:(id:string)=>disconnectCampaigns(id,idempotencyKey()),onSuccess:()=>{setDisconnectFor(null);setError(false);void qc.invalidateQueries({queryKey:['marketing','setup']});},onError:()=>setError(true)});
 if(!admin)return null;
 if(setup.isPending)return <Card><CardContent className="py-5 text-sm text-muted-foreground" aria-busy="true">{t('loading')}</CardContent></Card>;
 if(setup.isError)return <Card><CardContent className="py-4"><ErrorState error={setup.error} onRetry={()=>setup.refetch()}/></CardContent></Card>;
 const data=setup.data.data;
 return <Card>
   <CardHeader className="flex flex-row flex-wrap items-start justify-between gap-4">
    <div className="grid gap-1.5"><CardTitle>{t('title')}</CardTitle><CardDescription>{t('description')}</CardDescription></div>
    <Badge variant="secondary">{t('freePlan')}</Badge>
   </CardHeader>
   <CardContent className="flex flex-col gap-4">
    {!data.configured?<Alert variant="warning"><AlertTitle>{t('setupTitle')}</AlertTitle><AlertDescription>
      <p>{t('setupDescription')}</p><div className="mt-2 flex flex-wrap gap-2">{data.missing_fields.map(field=><code key={field} className="rounded border bg-background px-2 py-1 font-mono text-2xs">{field}</code>)}</div>
    </AlertDescription></Alert>:null}
    <div className="grid gap-2"><p className="text-xs font-medium">{t('callbackLabel')}</p><CopyField value={data.callback_uri} label={t('callbackLabel')}/><p className="text-2xs text-muted-foreground">{t('callbackHint')}</p></div>
    <details className="rounded-lg border px-3 py-2"><summary className="cursor-pointer text-sm font-medium">{t('instructions')}</summary>
     <ol className="mt-3 list-decimal space-y-2 pl-5 text-sm text-muted-foreground"><li>{t('step1')} <a href="https://api-console.zoho.com/" target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-foreground underline underline-offset-2">Zoho API Console<ExternalLinkIcon className="size-3" aria-hidden/></a></li><li>{t('step2')}</li><li>{t('step3')}</li><li>{t('step4')}</li></ol>
     <p className="mt-3 text-2xs text-muted-foreground">{t('securityHint')}</p>
    </details>
    <div className="flex flex-wrap items-center justify-between gap-3"><p className="text-2xs text-muted-foreground">{t('capabilities')}</p>
      <Button type="button" size="sm" disabled={!data.configured||connect.isPending} onClick={()=>{setError(false);connect.mutate()}}><LinkIcon aria-hidden/>{connect.isPending?t('connecting'):t('connect')}</Button>
    </div>
    {error?<p role="alert" className="text-sm text-destructive">{t('requestFailed')}</p>:null}
    {data.connections.length?<div className="divide-y rounded-lg border">{data.connections.map(connection=><div key={connection.id} className="flex flex-wrap items-center justify-between gap-3 px-3 py-3">
      <div><p className="text-sm font-medium">{connection.name}</p><p className="mt-1 text-2xs text-muted-foreground">{t.has('statuses.'+connection.status)?t('statuses.'+connection.status):connection.status} · {t('listCount',{count:connection.lists_count})}</p></div>
      <div className="flex flex-wrap gap-2">{connection.status==='active'?<>
       <Button type="button" variant="outline" size="sm" disabled={recent.isPending} onClick={()=>{setRecentCampaignsFor(connection);setRecentCampaigns(null);setRecentCampaignsError(false);recent.mutate(connection.id)}}><RefreshCwIcon aria-hidden/>{t('recentCampaigns')}</Button>
       <Button asChild variant="ghost" size="sm"><a href={data.campaigns_host} target="_blank" rel="noopener noreferrer">{t('openZohoCampaigns')}<ExternalLinkIcon aria-hidden/></a></Button>
      </>:null}<Button type="button" variant="outline" size="sm" disabled={connection.status!=='active'||verify.isPending} onClick={()=>verify.mutate(connection.id)}><RefreshCwIcon aria-hidden/>{t('verify')}</Button><Button type="button" variant="ghost" size="sm" onClick={()=>setDisconnectFor(connection)}>{t('disconnect')}</Button></div>
    </div>)}</div>:null}
    <Dialog open={authorization!==null} onOpenChange={open=>{if(!open)setAuthorization(null)}}><DialogContent><DialogHeader><DialogTitle>{t('authorizeTitle')}</DialogTitle><DialogDescription>{t('authorizeDescription')}</DialogDescription></DialogHeader>
      {authorization?<Button asChild><a href={authorization} target="_blank" rel="noopener noreferrer">{t('authorizeOpen')}<ExternalLinkIcon aria-hidden/></a></Button>:null}
      <DialogFooter><Button type="button" variant="outline" onClick={()=>{setAuthorization(null);void setup.refetch()}}>{t('close')}</Button></DialogFooter>
    </DialogContent></Dialog>
    <Dialog open={lists!==null} onOpenChange={open=>{if(!open)setLists(null)}}><DialogContent><DialogHeader><DialogTitle>{t('listsTitle')}</DialogTitle><DialogDescription>{t('listsHint')}</DialogDescription></DialogHeader>
     {lists?.length?<ul className="max-h-64 divide-y overflow-auto rounded-lg border">{lists.map(list=><li key={list.key} className="flex justify-between gap-3 px-3 py-3 text-sm"><span>{list.name}</span><span className="text-muted-foreground">{t('contacts',{count:list.contacts})}</span></li>)}</ul>:<p className="text-sm text-muted-foreground">{t('noLists')}</p>}
    </DialogContent></Dialog>
    <Dialog open={recentCampaignsFor!==null} onOpenChange={open=>{if(!open){setRecentCampaignsFor(null);setRecentCampaigns(null);setRecentCampaignsError(false)}}}><DialogContent><DialogHeader><DialogTitle>{t('recentCampaignsTitle')}</DialogTitle><DialogDescription>{t('recentCampaignsHint')}</DialogDescription></DialogHeader>
     {recent.isPending?<p className="text-sm text-muted-foreground" role="status">{t('recentCampaignsLoading')}</p>:recentCampaignsError?<p className="text-sm text-destructive" role="alert">{t('recentCampaignsFailed')}</p>:recentCampaigns?.length?<ul className="max-h-64 divide-y overflow-auto rounded-lg border">{recentCampaigns.map(campaign=><li key={campaign.key} className="flex justify-between gap-3 px-3 py-3 text-sm"><span>{campaign.name}</span><Badge variant="secondary">{campaign.status}</Badge></li>)}</ul>:<p className="text-sm text-muted-foreground">{t('noRecentCampaigns')}</p>}
     <DialogFooter><Button type="button" variant="outline" onClick={()=>{setRecentCampaignsFor(null);setRecentCampaigns(null);setRecentCampaignsError(false)}}>{t('close')}</Button></DialogFooter>
    </DialogContent></Dialog>
    <Dialog open={disconnectFor!==null} onOpenChange={open=>{if(!open)setDisconnectFor(null)}}><DialogContent><DialogHeader><DialogTitle>{t('disconnectTitle')}</DialogTitle><DialogDescription>{t('disconnectHint')}</DialogDescription></DialogHeader><DialogFooter><Button type="button" variant="outline" onClick={()=>setDisconnectFor(null)}>{t('cancel')}</Button><Button type="button" variant="destructive" disabled={disconnect.isPending} onClick={()=>{if(disconnectFor)disconnect.mutate(disconnectFor.id)}}>{t('disconnect')}</Button></DialogFooter></DialogContent></Dialog>
   </CardContent>
 </Card>
}
