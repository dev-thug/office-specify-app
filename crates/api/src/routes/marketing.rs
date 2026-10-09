use crate::{
    extractors::{AuthCtx, IdempotencyKey},
    problem::ProblemError,
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Redirect,
};
use contracts::marketing::*;
use contracts::{AppError, Envelope, Meta, Problem};
use infrastructure::idempotency;
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_setup))
        .routes(routes!(sender_identities))
        .routes(routes!(register_sender))
        .routes(routes!(contact_fields))
        .routes(routes!(topics))
        .routes(routes!(connect))
        .routes(routes!(callback))
        .routes(routes!(list_lists))
        .routes(routes!(recent_campaigns))
        .routes(routes!(disconnect))
}
fn envelope<T: serde::Serialize>(data: T, id: &str) -> Envelope<T> {
    Envelope {
        data,
        meta: Meta {
            request_id: id.into(),
        },
        page: None,
    }
}
#[utoipa::path(get,path="/integrations/zoho-campaigns",responses((status=200,body=CampaignsSetupResponse),(status=403,body=Problem)))]
async fn get_setup(
    State(state): State<AppState>,
    auth: AuthCtx,
) -> Result<Json<Envelope<CampaignsSetupResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.oauth_setup").await?;
    Ok(Json(envelope(
        application::marketing::setup(&state.db, &ctx, &state.config).await?,
        &ctx.request_id,
    )))
}
#[utoipa::path(post,path="/integrations/zoho-campaigns/connect",responses((status=201,body=MarketingConnectResponse),(status=422,body=Problem),(status=403,body=Problem)))]
async fn connect(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<(StatusCode, Json<Envelope<MarketingConnectResponse>>), ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.connection_admin").await?;
    state.config.zoho.campaigns.validate()?;
    if let Some(limiter) = &state.rate_limiter {
        limiter
            .require(
                &format!("campaigns-oauth:{}:{}", ctx.workspace_id, ctx.principal_id),
                5,
                std::time::Duration::from_secs(900),
                infrastructure::ratelimit::FailMode::Closed,
            )
            .await?;
    }
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/integrations/zoho-campaigns/connect".into(),
        key,
    };
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let outcome = idempotency::run(
        &state.db,
        &scope,
        &infrastructure::crypto::sha256_bytes(b"campaigns-oauth-start-v1"),
        {
            let ctx = ctx.clone();
            let config = state.config.clone();
            let captured = captured.clone();
            move |mut tx| {
                let ctx = ctx.clone();
                let config = config.clone();
                let captured = captured.clone();
                Box::pin(async move {
                    let mut response =
                        application::marketing::start_tx(&mut tx, &ctx, &config).await?;
                    *captured.lock().unwrap() = response.authorization_url.take();
                    Ok((
                        tx,
                        idempotency::Stored {
                            status: 201,
                            body: response,
                        },
                    ))
                })
            }
        },
    )
    .await?;
    let replay = outcome.is_replay();
    let mut stored = outcome.into_inner();
    if !replay {
        stored.body.authorization_url = captured.lock().unwrap().take();
    }
    Ok((
        StatusCode::CREATED,
        Json(envelope(stored.body, &ctx.request_id)),
    ))
}
#[derive(Deserialize)]
struct CallbackQuery {
    state: String,
    code: Option<String>,
    error: Option<String>,
    location: Option<String>,
    #[serde(rename = "accounts-server")]
    accounts_server: Option<String>,
}
#[utoipa::path(get,path="/integrations/zoho-campaigns/callback",responses((status=303,description="Return to settings"),(status=422,body=Problem),(status=401,body=Problem)))]
async fn callback(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(query): Query<CallbackQuery>,
) -> Result<Redirect, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.oauth_callback").await?;
    if query
        .location
        .as_deref()
        .is_some_and(|l| l != "us" && l != "com")
        || query
            .accounts_server
            .as_deref()
            .is_some_and(|s| s != "https://accounts.zoho.com")
    {
        return Err(AppError::validation(
            "This Campaigns connection supports the US data center only",
            vec![],
        )
        .into());
    }
    let provider = state.campaigns.as_ref().ok_or_else(|| {
        AppError::validation("Configure Campaigns OAuth before connecting", vec![])
    })?;
    application::marketing::callback(
        &state.db,
        &ctx,
        &state.config,
        provider,
        &query.state,
        query.code.as_deref(),
        query.error.is_some(),
    )
    .await?;
    let mut target = infrastructure::providers::zoho_campaigns::http::ProviderUrl::parse(
        state.config.zoho.campaigns.redirect_uri.as_deref().unwrap(),
    )
    .map_err(|_| AppError::internal("invalid configured callback"))?;
    target.set_path("/app/settings/mail-accounts");
    target.set_query(Some("zoho_campaigns=connected"));
    Ok(Redirect::to(target.as_str()))
}
#[utoipa::path(get,path="/marketing/connections/{id}/lists",params(("id"=Uuid,Path)),responses((status=200,body=Vec<MarketingListResponse>),(status=404,body=Problem),(status=403,body=Problem)))]
async fn list_lists(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<Json<Envelope<Vec<MarketingListResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.connection_admin").await?;
    let provider = state
        .campaigns
        .as_ref()
        .ok_or_else(|| AppError::validation("Campaigns OAuth is not configured", vec![]))?;
    Ok(Json(envelope(
        application::marketing::list_lists(&state.db, &ctx, &state.config, provider, id).await?,
        &ctx.request_id,
    )))
}
#[utoipa::path(post,path="/marketing/connections/{id}/disconnect",params(("id"=Uuid,Path)),responses((status=200,body=MarketingDisconnectedResponse),(status=404,body=Problem),(status=403,body=Problem)))]
async fn disconnect(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<Json<Envelope<MarketingDisconnectedResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.connection_admin").await?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/marketing/connections/{id}/disconnect".into(),
        key,
    };
    let outcome = idempotency::run(
        &state.db,
        &scope,
        &infrastructure::crypto::sha256_bytes(id.as_bytes()),
        {
            let ctx = ctx.clone();
            move |mut tx| {
                let ctx = ctx.clone();
                Box::pin(async move {
                    let body = application::marketing::disconnect_tx(&mut tx, &ctx, id).await?;
                    Ok((tx, idempotency::Stored { status: 200, body }))
                })
            }
        },
    )
    .await?;
    application::marketing::cleanup_disconnected(&state.db, &ctx, &state.config, id).await?;
    Ok(Json(envelope(outcome.into_inner().body, &ctx.request_id)))
}

#[utoipa::path(get,path="/marketing/connections/{id}/campaigns",params(("id"=Uuid,Path)),responses((status=200,body=Vec<NativeCampaignResponse>),(status=404,body=Problem),(status=403,body=Problem)))]
async fn recent_campaigns(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<Json<Envelope<Vec<NativeCampaignResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.campaigns_read").await?;
    let provider = state
        .campaigns
        .as_ref()
        .ok_or_else(|| AppError::validation("Campaigns OAuth is not configured", vec![]))?;
    Ok(Json(envelope(
        application::marketing::recent_campaigns(&state.db, &ctx, &state.config, provider, id)
            .await?,
        &ctx.request_id,
    )))
}

#[derive(Deserialize)]
struct SenderQuery {
    mail_account_id: Uuid,
}
#[utoipa::path(get,path="/marketing/sender-identities",params(("mail_account_id"=Uuid, Query)),responses((status=200,body=[MarketingSenderResponse]),(status=403,body=Problem)))]
async fn sender_identities(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(query): Query<SenderQuery>,
) -> Result<Json<Envelope<Vec<MarketingSenderResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let rows =
        application::marketing::sender_identities(&state.db, &ctx, query.mail_account_id).await?;
    Ok(Json(envelope(rows, &ctx.request_id)))
}

#[utoipa::path(post,path="/marketing/sender-identities",request_body=RegisterMarketingSenderRequest,responses((status=201,body=MarketingSenderResponse),(status=403,body=Problem),(status=422,body=Problem)))]
async fn register_sender(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(request): Json<RegisterMarketingSenderRequest>,
) -> Result<(StatusCode, Json<Envelope<MarketingSenderResponse>>), ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize_sender_registration(&state.db, &ctx, request.mail_account_id)
        .await?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/marketing/sender-identities".into(),
        key,
    };
    let hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&request)
            .map_err(|_| AppError::internal("could not encode sender request"))?,
    );
    let saved = idempotency::run(&state.db, &scope, &hash, {
        let ctx = ctx.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            let request = request.clone();
            Box::pin(async move {
                let body =
                    application::marketing::register_sender_tx(&mut tx, &ctx, &request).await?;
                Ok((tx, idempotency::Stored { status: 201, body }))
            })
        }
    })
    .await?
    .into_inner();
    Ok((
        StatusCode::CREATED,
        Json(envelope(saved.body, &ctx.request_id)),
    ))
}

#[utoipa::path(get,path="/marketing/connections/{id}/fields",params(("id"=Uuid,Path)),responses((status=200,body=[MarketingContactFieldResponse]),(status=403,body=Problem),(status=503,body=Problem)))]
async fn contact_fields(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<Json<Envelope<Vec<MarketingContactFieldResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let data = application::marketing::contact_fields(
        &state.db,
        &ctx,
        &state.config,
        state.campaigns.as_deref(),
        state.operations.as_deref(),
        id,
    )
    .await?;
    Ok(Json(envelope(data, &ctx.request_id)))
}
#[derive(Deserialize)]
struct TopicQuery {
    from_index: Option<u32>,
    limit: Option<u32>,
}
#[utoipa::path(get,path="/marketing/connections/{id}/topics",params(("id"=Uuid,Path),("from_index"=Option<u32>,Query),("limit"=Option<u32>,Query)),responses((status=200,body=[MarketingTopicResponse]),(status=403,body=Problem),(status=503,body=Problem)))]
async fn topics(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(query): Query<TopicQuery>,
) -> Result<Json<Envelope<Vec<MarketingTopicResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let data = application::marketing::topics(
        &state.db,
        &ctx,
        &state.config,
        state.campaigns.as_deref(),
        state.operations.as_deref(),
        id,
        query.from_index.unwrap_or(1),
        query.limit.unwrap_or(100),
    )
    .await?;
    Ok(Json(envelope(data, &ctx.request_id)))
}
