//! crm module routes (spec §6 — owned by the module worker).
//!
//! B.2 CRM rows: `/companies` (list/create/get/patch/archive/merge-requests/
//! activities), `/contacts` (+emails add/remove, primary-email), `/notes`,
//! `/tasks`. Paging, version and idempotency headers use shared extractors.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use contracts::crm::{
    AddContactEmailRequest, ArchiveCompanyRequest, CreateCompanyRequest, CreateContactRequest,
    CreateNoteRequest, CreateTaskRequest, MergeCompanyRequest, PatchCompanyRequest,
    PatchContactRequest, PatchTaskRequest, SetPrimaryEmailRequest,
};
use contracts::{Envelope, Problem};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extract::PageParams;
use crate::extractors::{AuthCtx, IdempotencyKey, IfMatch, Json, OptionalIdempotencyKey};
use crate::problem::ProblemError;
use crate::state::AppState;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

/// Query structs remain in OpenAPI for their filter parameters; this checks
/// that the shared paging extractor parsed the same query values, including
/// its documented max-limit clamp.
fn page_from_shared(limit: u32, cursor: Option<&str>, page: PageParams) -> PageParams {
    let _ = (limit, cursor, MAX_LIMIT);
    debug_assert_eq!(limit.min(MAX_LIMIT), page.limit);
    debug_assert_eq!(cursor, page.cursor.as_deref());
    page
}

#[derive(Debug, Deserialize, IntoParams)]
struct CompaniesQuery {
    /// Free-text search over name and domain.
    q: Option<String>,
    /// DDL lifecycle_stage values.
    lifecycle_stage: Option<String>,
    owner_principal_id: Option<Uuid>,
    /// true = archived only; absent/false = active (default view).
    archived: Option<bool>,
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
struct ContactsQuery {
    q: Option<String>,
    company_id: Option<Uuid>,
    status: Option<String>,
    owner_principal_id: Option<Uuid>,
    archived: Option<bool>,
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
struct NotesQuery {
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    thread_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
struct TasksQuery {
    status: Option<String>,
    owner_principal_id: Option<Uuid>,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
struct ActivitiesQuery {
    #[serde(default = "default_limit")]
    limit: u32,
    cursor: Option<String>,
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_companies))
        .routes(routes!(create_company))
        .routes(routes!(get_company))
        .routes(routes!(patch_company))
        .routes(routes!(archive_company))
        .routes(routes!(request_company_merge))
        .routes(routes!(list_company_activities))
        .routes(routes!(list_contacts))
        .routes(routes!(create_contact))
        .routes(routes!(get_contact))
        .routes(routes!(patch_contact))
        .routes(routes!(add_contact_email))
        .routes(routes!(remove_contact_email))
        .routes(routes!(set_primary_email))
        .routes(routes!(create_note))
        .routes(routes!(list_notes))
        .routes(routes!(list_tasks))
        .routes(routes!(create_task))
        .routes(routes!(patch_task))
}

fn etag(version: i64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{version}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("\"1\""))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id.to_string())
}

fn stored<T: serde::Serialize>(
    stored: infrastructure::idempotency::Stored<T>,
    request_id: &str,
) -> (StatusCode, axum::Json<Envelope<T>>) {
    let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    (status, axum::Json(envelope(stored.body, request_id)))
}

// --------------------------------------------------------------- companies

#[utoipa::path(
    get, path = "/companies", params(CompaniesQuery),
    responses(
        (status = 200, description = "Company page", body = Vec<contracts::crm::CompanyResponse>),
        (status = 400, description = "Invalid filter/cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_companies(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<CompaniesQuery>,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<contracts::crm::CompanyResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let p = page_from_shared(q.limit, q.cursor.as_deref(), page);
    let (rows, page) = application::crm::companies::list(
        &state.db,
        &ctx,
        &state.config.hmac_key,
        &application::crm::companies::CompanyListQuery {
            q: q.q,
            lifecycle_stage: q.lifecycle_stage,
            owner_principal_id: q.owner_principal_id,
            archived: q.archived.unwrap_or(false),
            limit: p.limit,
            cursor: p.cursor,
        },
    )
    .await?;
    Ok(axum::Json(envelope(rows, &request_id).with_page(page)))
}

#[utoipa::path(
    post, path = "/companies",
    request_body = CreateCompanyRequest,
    params(("Idempotency-Key" = String, Header, description = "Idempotent create key")),
    responses(
        (status = 201, description = "Created; duplicate_candidates advisory", body = contracts::crm::CompanyResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Agent/scope forbidden", body = Problem),
        (status = 409, description = "Idempotency-Key reused", body = Problem),
    )
)]
async fn create_company(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(req): Json<CreateCompanyRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::crm::companies::create(&state.db, &ctx, &req, key.0).await?;
    Ok(stored(out, &request_id))
}

#[utoipa::path(
    get, path = "/companies/{id}", params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Company (ETag = version)", body = contracts::crm::CompanyResponse),
        (status = 404, description = "Not found / other workspace", body = Problem),
    )
)]
async fn get_company(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::companies::get(&state.db, &ctx, id).await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    patch, path = "/companies/{id}", params(("id" = Uuid, Path)),
    request_body = PatchCompanyRequest,
    responses(
        (status = 200, description = "Updated", body = contracts::crm::CompanyResponse),
        (status = 404, description = "Not found", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_company(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchCompanyRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::companies::patch(&state.db, &ctx, id, if_match.0, &req).await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/companies/{id}/archive", params(("id" = Uuid, Path)),
    request_body = ArchiveCompanyRequest,
    responses(
        (status = 200, description = "Archived", body = contracts::crm::CompanyResponse),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "Already archived", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn archive_company(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<ArchiveCompanyRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::companies::archive(&state.db, &ctx, id, if_match.0, &req).await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/companies/{id}/merge-requests",
    params(("id" = Uuid, Path), ("Idempotency-Key" = String, Header)),
    request_body = MergeCompanyRequest,
    responses(
        (status = 202, description = "Merge job queued", body = contracts::crm::MergeCompanyResponse),
        (status = 400, description = "Invalid field_choices / self-merge", body = Problem),
        (status = 403, description = "Requires manager/admin", body = Problem),
        (status = 404, description = "Company not found", body = Problem),
    )
)]
async fn request_company_merge(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<MergeCompanyRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::crm::companies::request_merge(&state.db, &ctx, id, &req, key.0).await?;
    Ok(stored(out, &request_id))
}

#[utoipa::path(
    get, path = "/companies/{id}/activities",
    params(("id" = Uuid, Path), ActivitiesQuery),
    responses(
        (status = 200, description = "Activity page (mailbox-grant filtered)", body = Vec<contracts::crm::ActivityResponse>),
        (status = 404, description = "Company not found", body = Problem),
    )
)]
async fn list_company_activities(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<ActivitiesQuery>,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<contracts::crm::ActivityResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let p = page_from_shared(q.limit, q.cursor.as_deref(), page);
    let (rows, page) = application::crm::activities::list_company(
        &state.db,
        &ctx,
        &state.config.hmac_key,
        id,
        &application::crm::activities::ActivityListQuery {
            limit: p.limit,
            cursor: p.cursor,
        },
    )
    .await?;
    Ok(axum::Json(envelope(rows, &request_id).with_page(page)))
}

// ---------------------------------------------------------------- contacts

#[utoipa::path(
    get, path = "/contacts", params(ContactsQuery),
    responses(
        (status = 200, description = "Contact page", body = Vec<contracts::crm::ContactResponse>),
        (status = 400, description = "Invalid filter/cursor", body = Problem),
    )
)]
async fn list_contacts(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<ContactsQuery>,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<contracts::crm::ContactResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let p = page_from_shared(q.limit, q.cursor.as_deref(), page);
    let (rows, page) = application::crm::contacts::list(
        &state.db,
        &ctx,
        &state.config.hmac_key,
        &application::crm::contacts::ContactListQuery {
            q: q.q,
            company_id: q.company_id,
            status: q.status,
            owner_principal_id: q.owner_principal_id,
            archived: q.archived.unwrap_or(false),
            limit: p.limit,
            cursor: p.cursor,
        },
    )
    .await?;
    Ok(axum::Json(envelope(rows, &request_id).with_page(page)))
}

#[utoipa::path(
    post, path = "/contacts",
    request_body = CreateContactRequest,
    responses(
        (status = 201, description = "Created", body = contracts::crm::ContactResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Agent/scope forbidden", body = Problem),
    )
)]
async fn create_contact(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: OptionalIdempotencyKey,
    Json(req): Json<CreateContactRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::crm::contacts::create(&state.db, &ctx, &req, key.0).await?;
    Ok(stored(out, &request_id))
}

#[utoipa::path(
    get, path = "/contacts/{id}", params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Contact + email context (ETag = version)", body = contracts::crm::ContactResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_contact(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::contacts::get(&state.db, &ctx, id).await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    patch, path = "/contacts/{id}", params(("id" = Uuid, Path)),
    request_body = PatchContactRequest,
    responses(
        (status = 200, description = "Updated", body = contracts::crm::ContactResponse),
        (status = 404, description = "Not found", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_contact(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchContactRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::contacts::patch(&state.db, &ctx, id, if_match.0, &req).await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/contacts/{id}/emails", params(("id" = Uuid, Path)),
    request_body = AddContactEmailRequest,
    responses(
        (status = 201, description = "Address linked (B.3.4)", body = contracts::crm::AddContactEmailResponse),
        (status = 400, description = "Invalid address", body = Problem),
        (status = 404, description = "Contact not found", body = Problem),
    )
)]
async fn add_contact_email(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: OptionalIdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<AddContactEmailRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::crm::contacts::add_email(&state.db, &ctx, id, &req, key.0).await?;
    Ok(stored(out, &request_id))
}

#[utoipa::path(
    delete, path = "/contacts/{id}/emails/{email_address_id}",
    params(("id" = Uuid, Path), ("email_address_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Link removed; updated contact", body = contracts::crm::ContactResponse),
        (status = 404, description = "Contact/link not found", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn remove_contact_email(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path((id, email_address_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res =
        application::crm::contacts::remove_email(&state.db, &ctx, id, email_address_id, if_match.0)
            .await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    put, path = "/contacts/{id}/primary-email", params(("id" = Uuid, Path)),
    request_body = SetPrimaryEmailRequest,
    responses(
        (status = 200, description = "Primary address switched", body = contracts::crm::ContactResponse),
        (status = 400, description = "Address not linked", body = Problem),
        (status = 404, description = "Contact not found", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn set_primary_email(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<SetPrimaryEmailRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::contacts::set_primary_email(&state.db, &ctx, id, if_match.0, &req)
        .await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}

// ------------------------------------------------------------------- notes

#[utoipa::path(
    post, path = "/notes",
    request_body = CreateNoteRequest,
    responses(
        (status = 201, description = "Created", body = contracts::crm::NoteResponse),
        (status = 400, description = "Exactly one target required", body = Problem),
        (status = 404, description = "Target not found / mailbox denied", body = Problem),
    )
)]
async fn create_note(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: OptionalIdempotencyKey,
    Json(req): Json<CreateNoteRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::crm::notes::create(&state.db, &ctx, &req, key.0).await?;
    Ok(stored(out, &request_id))
}

#[utoipa::path(
    get, path = "/notes", params(NotesQuery),
    responses(
        (status = 200, description = "Note page (mailbox-grant filtered)", body = Vec<contracts::crm::NoteResponse>),
        (status = 400, description = "Invalid filter/cursor", body = Problem),
    )
)]
async fn list_notes(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<NotesQuery>,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<contracts::crm::NoteResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let p = page_from_shared(q.limit, q.cursor.as_deref(), page);
    let (rows, page) = application::crm::notes::list(
        &state.db,
        &ctx,
        &state.config.hmac_key,
        &application::crm::notes::NoteListQuery {
            company_id: q.company_id,
            contact_id: q.contact_id,
            thread_id: q.thread_id,
            limit: p.limit,
            cursor: p.cursor,
        },
    )
    .await?;
    Ok(axum::Json(envelope(rows, &request_id).with_page(page)))
}

// ------------------------------------------------------------------- tasks

#[utoipa::path(
    get, path = "/tasks", params(TasksQuery),
    responses(
        (status = 200, description = "Task page (member-scoped)", body = Vec<contracts::crm::TaskResponse>),
        (status = 400, description = "Invalid filter/cursor", body = Problem),
    )
)]
async fn list_tasks(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<TasksQuery>,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<contracts::crm::TaskResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let p = page_from_shared(q.limit, q.cursor.as_deref(), page);
    let (rows, page) = application::crm::tasks::list(
        &state.db,
        &ctx,
        &state.config.hmac_key,
        &application::crm::tasks::TaskListQuery {
            status: q.status,
            owner_principal_id: q.owner_principal_id,
            company_id: q.company_id,
            contact_id: q.contact_id,
            limit: p.limit,
            cursor: p.cursor,
        },
    )
    .await?;
    Ok(axum::Json(envelope(rows, &request_id).with_page(page)))
}

#[utoipa::path(
    post, path = "/tasks",
    request_body = CreateTaskRequest,
    responses(
        (status = 201, description = "Created", body = contracts::crm::TaskResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Reassignment forbidden", body = Problem),
    )
)]
async fn create_task(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: OptionalIdempotencyKey,
    Json(req): Json<CreateTaskRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::crm::tasks::create(&state.db, &ctx, &req, key.0).await?;
    Ok(stored(out, &request_id))
}

#[utoipa::path(
    patch, path = "/tasks/{id}", params(("id" = Uuid, Path)),
    request_body = PatchTaskRequest,
    responses(
        (status = 200, description = "Updated", body = contracts::crm::TaskResponse),
        (status = 403, description = "Not owner/manager", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_task(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchTaskRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::crm::tasks::patch(&state.db, &ctx, id, if_match.0, &req).await?;
    Ok((
        [(header::ETAG, etag(res.version))],
        axum::Json(envelope(res, &request_id)),
    ))
}
