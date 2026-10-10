"use client";
import {useRef,useState,type FormEvent} from 'react';
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
import {ApiError} from '@/lib/api/problem';
import {createChannelReview,patchDeliveryChannel,useDeliveryChannels,useMailAccounts,useChannelReviews,type DeliveryChannel} from '@/lib/api/settings';
import {createMarketingChannel,registerMarketingSender,useCampaignsSetup,connectCampaigns,getCampaignsLists,getRecentCampaigns,disconnectCampaigns,useMarketingSenderIdentities,type MarketingList,type MarketingConnection,type NativeCampaign} from '@/lib/api/marketing';
import {Input} from '@/components/ui/input';
import {Label} from '@/components/ui/label';
import {Textarea} from '@/components/ui/textarea';
import {Select,SelectContent,SelectItem,SelectTrigger,SelectValue} from '@/components/ui/select';

export function CampaignsConnectionSettings(){
 const t=useTranslations('settings.campaigns');const admin=useIsAdmin();const qc=useQueryClient();
 const setup=useCampaignsSetup(admin);const [authorization,setAuthorization]=useState<string|null>(null);
 const [lists,setLists]=useState<MarketingList[]|null>(null);const [disconnectFor,setDisconnectFor]=useState<MarketingConnection|null>(null);
 const [recentCampaignsFor,setRecentCampaignsFor]=useState<MarketingConnection|null>(null);
 const [recentCampaigns,setRecentCampaigns]=useState<NativeCampaign[]|null>(null);const [recentCampaignsError,setRecentCampaignsError]=useState(false);
 const [error,setError]=useState(false);
 const connectKey=useRef(idempotencyKey());const disconnectKeys=useRef(new Map<string,string>());
 const connect=useMutation({mutationFn:()=>connectCampaigns(connectKey.current),onSuccess:result=>{if(result.data.authorization_url)connectKey.current=idempotencyKey();setAuthorization(result.data.authorization_url ?? null);setError(!result.data.authorization_url);void qc.invalidateQueries({queryKey:['marketing','setup']});},onError:cause=>{setError(true);if(cause instanceof ApiError&&cause.status<500&&cause.problem.retryable!==true)connectKey.current=idempotencyKey();}});
 const verify=useMutation({mutationFn:(id:string)=>getCampaignsLists(id),onSuccess:result=>{setLists(result.data);setError(false);void qc.invalidateQueries({queryKey:['marketing','setup']});},onError:()=>setError(true)});
 const recent=useMutation({mutationFn:(id:string)=>getRecentCampaigns(id),onSuccess:result=>{setRecentCampaigns(result.data);setRecentCampaignsError(false);},onError:()=>setRecentCampaignsError(true)});
 const disconnect=useMutation({mutationFn:(id:string)=>{const key=disconnectKeys.current.get(id)??idempotencyKey();disconnectKeys.current.set(id,key);return disconnectCampaigns(id,key)},onSuccess:(_result,id)=>{disconnectKeys.current.delete(id);setDisconnectFor(null);setError(false);void qc.invalidateQueries({queryKey:['marketing','setup']});},onError:(cause,id)=>{setError(true);if(cause instanceof ApiError&&cause.status<500&&cause.problem.retryable!==true)disconnectKeys.current.delete(id);}});
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
    {data.connections.some(connection=>connection.status==='active'&&connection.write_scopes_ready!==true)?<Alert variant="warning"><AlertTitle>{t('writeScopeTitle')}</AlertTitle><AlertDescription className="flex flex-wrap items-center justify-between gap-3"><span>{t('writeScopeNotice')}</span><Button type="button" size="sm" variant="outline" disabled={!data.configured||connect.isPending} onClick={()=>{setError(false);connect.mutate()}}>{connect.isPending?t('connecting'):t('reconnectWriteScope')}</Button></AlertDescription></Alert>:null}
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
    <MarketingChannelSetup connections={data.connections} configured={data.configured}/>
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

function MarketingChannelSetup({connections,configured}:{connections:MarketingConnection[];configured:boolean}){
 const t=useTranslations('settings.campaigns');const tCommon=useTranslations('common');const qc=useQueryClient();
 const campaigns=connections.filter(connection=>connection.status==='active');
 const accountsQuery=useMailAccounts();const accountRows=accountsQuery.data?.pages.flatMap(page=>page.data)??[];
 const mailAccounts=accountRows.filter(account=>account.provider==='zoho_mail'&&account.status==='active');
 const channelQuery=useDeliveryChannels();const channelRows=channelQuery.data?.pages.flatMap(page=>page.data)??[];
 const channels=channelRows.filter(channel=>channel.provider==='zoho_campaigns'&&channel.purpose==='marketing');
 const [connectionId,setConnectionId]=useState('');const [mailAccountId,setMailAccountId]=useState('');
 const [senderIdentityId,setSenderIdentityId]=useState('');const [name,setName]=useState('');
 const [senderRegistration,setSenderRegistration]=useState({address:'',displayName:'',evidenceRef:'',confirmed:false});
 const [error,setError]=useState<string|null>(null);const [reviewFor,setReviewFor]=useState<DeliveryChannel|null>(null);
 const [historyFor,setHistoryFor]=useState<DeliveryChannel|null>(null);
 const [reviewEvidence,setReviewEvidence]=useState({policyUrl:'',evidenceRef:'',notes:'',confirmed:false});
 const createKey=useRef(idempotencyKey());const actionKeys=useRef(new Map<string,string>());
 const registerKey=useRef(idempotencyKey());
 const selectedConnectionId=campaigns.some(connection=>connection.id===connectionId)?connectionId:campaigns[0]?.id??'';
 const selectedMailAccountId=mailAccounts.some(candidate=>candidate.id===mailAccountId)?mailAccountId:mailAccounts[0]?.id??'';
 const senderQuery=useMarketingSenderIdentities(selectedMailAccountId||undefined,configured);
 const senderRows=senderQuery.data?.data??[];const verifiedSenders=senderRows.filter(sender=>sender.status==='verified');
 const selectedSenderIdentityId=verifiedSenders.some(sender=>sender.id===senderIdentityId)?senderIdentityId:verifiedSenders[0]?.id??'';
 const account=mailAccounts.find(candidate=>candidate.id===selectedMailAccountId);

 const create=useMutation({
  mutationFn:()=>createMarketingChannel({connection_id:selectedConnectionId,mail_account_id:selectedMailAccountId,sender_identity_id:selectedSenderIdentityId,purpose:'marketing',name:name.trim()},createKey.current),
  onSuccess:()=>{createKey.current=idempotencyKey();setError(null);setName('');void qc.invalidateQueries({queryKey:['operations','delivery-channels']});},
  onError:(cause)=>{setError(t('channelSetup.createFailed'));if(cause instanceof ApiError&&cause.status<500&&cause.problem.retryable!==true)createKey.current=idempotencyKey();},
 });
 const register=useMutation({
  mutationFn:()=>registerMarketingSender({mail_account_id:selectedMailAccountId,address:senderRegistration.address.trim(),display_name:senderRegistration.displayName.trim(),verification_evidence_ref:senderRegistration.evidenceRef.trim()},registerKey.current),
  onSuccess:()=>{registerKey.current=idempotencyKey();setSenderRegistration({address:'',displayName:'',evidenceRef:'',confirmed:false});setError(null);void qc.invalidateQueries({queryKey:['marketing','sender-identities',selectedMailAccountId]});},
  onError:(cause)=>{setError(t('channelSetup.senderRegisterFailed'));if(cause instanceof ApiError&&cause.status<500&&cause.problem.retryable!==true)registerKey.current=idempotencyKey();},
 });
 const review=useMutation({
  mutationFn:({channel,input,key}:{channel:DeliveryChannel;input:{result:'allowed';policy_url:string;contract_evidence_ref:string;notes:string};key:string})=>createChannelReview(channel.id,input,{idempotencyKey:key}),
  onSuccess:(_result,variables)=>{setError(null);setReviewFor(null);setReviewEvidence({policyUrl:'',evidenceRef:'',notes:'',confirmed:false});actionKeys.current.delete(`review:${variables.channel.id}`);void qc.invalidateQueries({queryKey:['operations','delivery-channels']});},
  onError:()=>setError(t('channelSetup.reviewFailed')),
 });
 const enable=useMutation({
  mutationFn:(channel:DeliveryChannel)=>patchDeliveryChannel(channel.id,{enabled:true},{etag:channel.version,idempotencyKey:keyFor(actionKeys.current,'enable',channel.id)}),
  onSuccess:(_result,channel)=>{setError(null);actionKeys.current.delete(`enable:${channel.id}`);void qc.invalidateQueries({queryKey:['operations','delivery-channels']});},
  onError:()=>{setError(t('channelSetup.enableFailed'));void qc.invalidateQueries({queryKey:['operations','delivery-channels']});},
 });

 const valid=Boolean(configured&&campaigns.some(connection=>connection.id===selectedConnectionId)&&account&&selectedSenderIdentityId&&verifiedSenders.some(sender=>sender.id===selectedSenderIdentityId)&&name.trim());
 function submitCreate(event:FormEvent<HTMLFormElement>){event.preventDefault();if(!valid||create.isPending)return;setError(null);create.mutate()}
 function submitRegister(event:FormEvent<HTMLFormElement>){event.preventDefault();if(!selectedMailAccountId||!senderRegistration.confirmed||!senderRegistration.address.trim()||!senderRegistration.displayName.trim()||!senderRegistration.evidenceRef.trim()||register.isPending)return;setError(null);register.mutate()}
 function submitReview(event:FormEvent<HTMLFormElement>){event.preventDefault();if(!reviewFor||!reviewEvidence.confirmed||!reviewEvidence.policyUrl.trim()||!reviewEvidence.evidenceRef.trim())return;const key=keyFor(actionKeys.current,'review',reviewFor.id);review.mutate({channel:reviewFor,input:{result:'allowed',policy_url:reviewEvidence.policyUrl.trim(),contract_evidence_ref:reviewEvidence.evidenceRef.trim(),notes:reviewEvidence.notes.trim()||t('channelSetup.reviewDefaultNote')},key})}

 return <section className="flex flex-col gap-3 border-t pt-4" aria-label={t('channelSetup.title')}>
  <div className="grid gap-1"><h3 className="text-sm font-semibold">{t('channelSetup.title')}</h3><p className="text-2xs text-muted-foreground">{t('channelSetup.description')}</p></div>
  {accountsQuery.isPending||channelQuery.isPending?<p role="status" className="text-sm text-muted-foreground">{t('channelSetup.loading')}</p>:null}
  {mailAccounts.length===0?<p className="text-2xs text-muted-foreground">{t('channelSetup.connectMailFirst')}</p>:null}
  {campaigns.length===0?<p className="text-2xs text-muted-foreground">{t('channelSetup.connectCampaignsFirst')}</p>:null}
  {mailAccounts.length>0?<form onSubmit={submitRegister} className="grid gap-3 rounded-lg border px-3 py-3 sm:grid-cols-2">
   <div className="grid gap-1 sm:col-span-2"><h4 className="text-sm font-medium">{t('channelSetup.registerSenderTitle')}</h4><p className="text-2xs text-muted-foreground">{t('channelSetup.registerSenderDescription')}</p></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-sender-address">{t('channelSetup.senderAddress')}</Label><Input id="marketing-sender-address" type="email" value={senderRegistration.address} onChange={event=>setSenderRegistration(value=>({...value,address:event.target.value}))} required/></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-sender-name">{t('channelSetup.senderDisplayName')}</Label><Input id="marketing-sender-name" value={senderRegistration.displayName} onChange={event=>setSenderRegistration(value=>({...value,displayName:event.target.value}))} required/></div>
   <div className="grid gap-1.5 sm:col-span-2"><Label htmlFor="marketing-sender-evidence">{t('channelSetup.senderEvidence')}</Label><Input id="marketing-sender-evidence" type="url" placeholder="https://" value={senderRegistration.evidenceRef} onChange={event=>setSenderRegistration(value=>({...value,evidenceRef:event.target.value}))} required/></div>
   <label className="flex items-start gap-2 text-2xs sm:col-span-2"><input type="checkbox" checked={senderRegistration.confirmed} onChange={event=>setSenderRegistration(value=>({...value,confirmed:event.target.checked}))} className="mt-0.5"/><span>{t('channelSetup.senderConfirmed')}</span></label>
   <div className="sm:col-span-2"><Button type="submit" size="sm" variant="outline" disabled={!selectedMailAccountId||!senderRegistration.confirmed||!senderRegistration.address.trim()||!senderRegistration.displayName.trim()||!senderRegistration.evidenceRef.trim()||register.isPending}>{register.isPending?t('channelSetup.registeringSender'):t('channelSetup.registerSender')}</Button></div>
  </form>:null}
  {campaigns.length>0&&mailAccounts.length>0?<form onSubmit={submitCreate} className="grid gap-3 rounded-lg border px-3 py-3 sm:grid-cols-2">
   <div className="grid gap-1.5"><Label htmlFor="marketing-channel-name">{t('channelSetup.name')}</Label><Input id="marketing-channel-name" value={name} onChange={event=>setName(event.target.value)} required/></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-connection">{t('channelSetup.connection')}</Label><Select value={selectedConnectionId} onValueChange={setConnectionId}><SelectTrigger id="marketing-connection"><SelectValue placeholder={t('channelSetup.connection')}/></SelectTrigger><SelectContent>{campaigns.map(connection=><SelectItem key={connection.id} value={connection.id}>{connection.name}</SelectItem>)}</SelectContent></Select></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-mail-account">{t('channelSetup.mailAccount')}</Label><Select value={selectedMailAccountId} onValueChange={value=>{setMailAccountId(value);setSenderIdentityId('')}}><SelectTrigger id="marketing-mail-account"><SelectValue placeholder={t('channelSetup.mailAccount')}/></SelectTrigger><SelectContent>{mailAccounts.map(account=><SelectItem key={account.id} value={account.id}>{account.name||account.primary_address}</SelectItem>)}</SelectContent></Select></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-sender">{t('channelSetup.verifiedSender')}</Label><Select value={selectedSenderIdentityId} onValueChange={setSenderIdentityId}><SelectTrigger id="marketing-sender"><SelectValue placeholder={senderQuery.isPending?t('channelSetup.loadingSenders'):t('channelSetup.verifiedSender')}/></SelectTrigger><SelectContent>{verifiedSenders.map(sender=><SelectItem key={sender.id} value={sender.id}>{sender.display_name} &lt;{sender.address}&gt;</SelectItem>)}</SelectContent></Select>{verifiedSenders.length===0&&!senderQuery.isPending?<p className="text-2xs text-destructive">{t('channelSetup.noVerifiedSender')}</p>:null}</div>
   <div className="sm:col-span-2"><Button type="submit" size="sm" disabled={!valid||create.isPending}>{create.isPending?t('channelSetup.creating'):t('channelSetup.create')}</Button><p className="mt-2 text-2xs text-muted-foreground">{t('channelSetup.createNotice')}</p></div>
  </form>:null}
  {channelQuery.isError?<ErrorState error={channelQuery.error} onRetry={()=>channelQuery.refetch()}/>:null}
  {channels.length>0?<ul className="divide-y rounded-lg border">{channels.map(channel=>{
   const verified=senderRows.some(sender=>sender.id===channel.sender_identity_id&&sender.status==='verified'&&channel.mail_account_id===selectedMailAccountId);
   const reviewPending=review.isPending&&review.variables?.channel.id===channel.id;const enablePending=enable.isPending&&enable.variables?.id===channel.id;
   return <li key={channel.id} className="flex flex-wrap items-center justify-between gap-3 px-3 py-3"><div><p className="text-sm font-medium">{channel.name}</p><p className="mt-1 text-2xs text-muted-foreground">{t('channelSetup.channelStatus',{policy:channel.policy_status,enabled:channel.enabled?t('channelSetup.enabled'):t('channelSetup.disabled')})} · {verified?t('channelSetup.senderVerified'):t('channelSetup.senderNeedsVerification')}</p></div><div className="flex flex-wrap gap-2"><Button type="button" size="xs" variant="outline" disabled={!verified||reviewPending||channel.policy_status==='allowed'} onClick={()=>{setReviewEvidence({policyUrl:'',evidenceRef:'',notes:'',confirmed:false});setReviewFor(channel)}}>{reviewPending?t('channelSetup.reviewing'):t('channelSetup.addEvidence')}</Button><Button type="button" size="xs" variant="outline" disabled={!verified||channel.policy_status!=='allowed'||channel.enabled||enablePending} onClick={()=>enable.mutate(channel)}>{enablePending?t('channelSetup.enabling'):t('channelSetup.enable')}</Button><ChannelReviewHistory channel={historyFor?.id===channel.id?historyFor:null} onOpen={()=>setHistoryFor(channel)} onClose={()=>setHistoryFor(null)}/></div></li>
  })}</ul>:null}
  {error?<p role="alert" className="text-sm text-destructive">{error}</p>:null}

  <Dialog open={reviewFor!==null} onOpenChange={open=>{if(!open)setReviewFor(null)}}><DialogContent><DialogHeader><DialogTitle>{t('channelSetup.reviewTitle')}</DialogTitle><DialogDescription>{t('channelSetup.reviewDescription',{name:reviewFor?.name??''})}</DialogDescription></DialogHeader><form onSubmit={submitReview} className="grid gap-3">
   <div className="grid gap-1.5"><Label htmlFor="marketing-policy-url">{t('channelSetup.policyUrl')}</Label><Input id="marketing-policy-url" type="url" value={reviewEvidence.policyUrl} onChange={event=>setReviewEvidence(value=>({...value,policyUrl:event.target.value}))} required/></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-evidence-ref">{t('channelSetup.evidenceRef')}</Label><Input id="marketing-evidence-ref" value={reviewEvidence.evidenceRef} onChange={event=>setReviewEvidence(value=>({...value,evidenceRef:event.target.value}))} required/></div>
   <div className="grid gap-1.5"><Label htmlFor="marketing-review-notes">{t('channelSetup.notes')}</Label><Textarea id="marketing-review-notes" rows={3} value={reviewEvidence.notes} onChange={event=>setReviewEvidence(value=>({...value,notes:event.target.value}))}/></div>
   <label className="flex items-start gap-2 text-2xs"><input type="checkbox" checked={reviewEvidence.confirmed} onChange={event=>setReviewEvidence(value=>({...value,confirmed:event.target.checked}))} className="mt-0.5"/><span>{t('channelSetup.confirmVerified')}</span></label>
   <DialogFooter><Button type="button" variant="outline" onClick={()=>setReviewFor(null)}>{tCommon('cancel')}</Button><Button type="submit" disabled={!reviewEvidence.confirmed||!reviewEvidence.policyUrl.trim()||!reviewEvidence.evidenceRef.trim()||review.isPending}>{review.isPending?t('channelSetup.reviewing'):t('channelSetup.submitReview')}</Button></DialogFooter>
  </form></DialogContent></Dialog>
 </section>
}

function keyFor(keys:Map<string,string>,action:string,id:string){const token=`${action}:${id}`;const existing=keys.get(token);if(existing)return existing;const created=idempotencyKey();keys.set(token,created);return created}

function ChannelReviewHistory({channel,onOpen,onClose}:{channel:DeliveryChannel|null;onOpen:()=>void;onClose:()=>void}){
 const t=useTranslations('settings.campaigns');const reviews=useChannelReviews(channel?.id);const rows=reviews.data?.pages.flatMap(page=>page.data)??[];
 return <><Button type="button" size="xs" variant="ghost" onClick={onOpen}>{t('channelSetup.reviewHistory')}</Button><Dialog open={channel!==null} onOpenChange={open=>!open&&onClose()}><DialogContent><DialogHeader><DialogTitle>{t('channelSetup.reviewHistory')}</DialogTitle><DialogDescription>{channel?.name}</DialogDescription></DialogHeader>{reviews.isPending?<p role="status" className="text-sm text-muted-foreground">{t('channelSetup.loading')}</p>:reviews.isError?<ErrorState error={reviews.error} onRetry={()=>reviews.refetch()}/>:rows.length?<ul className="divide-y rounded-lg border">{rows.map(review=><li key={review.id} className="grid gap-1 px-3 py-2 text-2xs"><span className="font-medium">{review.result} · {review.purpose}</span><span className="text-muted-foreground">{review.notes??'—'} · {review.reviewed_at}</span></li>)}</ul>:<p className="text-sm text-muted-foreground">{t('channelSetup.noReviews')}</p>}</DialogContent></Dialog></>
}
