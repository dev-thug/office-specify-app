//! KPI summaries from the approved §2.4 definitions.

use chrono::{DateTime, Duration, NaiveTime, Utc};
use chrono_tz::Tz;
use contracts::operations::{
    DashboardFilters, DashboardKpis, DashboardPeriod, DashboardQueue, DashboardSummaryResponse,
    MedianKpi, OperationalCounts, RatioKpi,
};
use contracts::{AppError, Ctx, PrincipalKind, Role, Scope};
use infrastructure::db::Db;
use infrastructure::repo::operations::{self as repo, KpiWindow};

fn ratio(numerator: i64, denominator: i64) -> RatioKpi {
    RatioKpi {
        numerator,
        denominator,
        value: (denominator > 0).then(|| numerator as f64 / denominator as f64),
    }
}

fn period(filters: &DashboardFilters) -> Result<(DateTime<Utc>, DateTime<Utc>), AppError> {
    let now = Utc::now();
    let to = filters.to.unwrap_or(now);
    let from = filters.from.unwrap_or(to - Duration::days(30));
    if from >= to || to > now + Duration::minutes(1) || to - from > Duration::days(366) {
        return Err(AppError::validation(
            "dashboard period must be ordered, not future-dated, and at most 366 days",
            vec![contracts::Violation {
                field: "from".into(),
                code: "invalid_range".into(),
            }],
        ));
    }
    Ok((from, to))
}

fn first_instant_of_local_day(date: chrono::NaiveDate, timezone: Tz) -> Option<DateTime<Utc>> {
    let midnight = date.and_time(NaiveTime::MIN);
    for minute in 0..(24 * 60) {
        let local = midnight + Duration::minutes(minute);
        if let Some(instant) = domain::booking::local_instants(local, timezone)
            .into_iter()
            .min()
        {
            return Some(instant);
        }
    }
    None
}

fn queue(key: &str, count: repo::DashboardQueueCount) -> DashboardQueue {
    DashboardQueue {
        key: key.into(),
        count: count.count,
        oldest_at: count.oldest_at,
    }
}

/// `GET /dashboard/summary`. Cohort KPIs use provider-accepted outbound
/// messages only, with their numerator and denominator drawn from the same
/// time window. Unknown delivery is never treated as success.
pub async fn summary(
    db: &Db,
    ctx: &Ctx,
    filters: &DashboardFilters,
) -> Result<DashboardSummaryResponse, AppError> {
    if !ctx.scopes.contains(Scope::OperationsRead) {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope operations.read",
        ));
    }
    if ctx.principal_kind == PrincipalKind::System {
        return Err(AppError::forbidden(
            "system principals cannot read the dashboard",
        ));
    }
    let can_view_team =
        (matches!(ctx.role, Role::Admin | Role::Manager) || crate::authz::agent_admin(ctx));
    let requested_scope =
        filters
            .scope
            .as_deref()
            .unwrap_or(if can_view_team { "team" } else { "me" });
    let scope = match requested_scope {
        "team" if can_view_team => "team",
        "me" | "mine" => "mine",
        _ => {
            return Err(AppError::forbidden(
                "requested dashboard scope is unavailable",
            ));
        }
    };
    let (from, to) = period(filters)?;
    let window = KpiWindow {
        from,
        to,
        requested_by: (scope == "mine").then_some(ctx.principal_id),
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let eligible = repo::eligible_contacted_companies(&mut tx, &window).await?;
    let replies = repo::reply_observed_companies(&mut tx, &window).await?;
    let meetings = repo::meeting_held_companies(&mut tx, &window).await?;
    let (sample_size, median_seconds) = repo::first_reply_median(&mut tx, &window).await?;
    let (adopted, reviewed) = repo::proposal_adoption(&mut tx, &window).await?;
    let counts = repo::operational_counts(&mut tx).await?;
    let mut queues = Vec::new();
    let mine = scope == "mine";

    if ctx.scopes.contains(Scope::MailRead) {
        if let Some(inbox) = repo::inbox_unhandled_queue(&mut tx, ctx.principal_id, mine).await? {
            queues.push(queue("inbox_unhandled", inbox));
        }
        let mailbox_health = repo::mailbox_health_queues(&mut tx, ctx.principal_id).await?;
        if mailbox_health.accessible {
            queues.push(queue("reauth_required", mailbox_health.reauth_required));
            queues.push(queue("sync_lag", mailbox_health.sync_lag));
        }
        if let Some(unknown) = repo::unknown_outbound_queue(
            &mut tx,
            ctx.principal_id,
            mine.then_some(ctx.principal_id),
        )
        .await?
        {
            queues.push(queue("outbound_unknown", unknown));
        }
    }

    if can_view_team && ctx.scopes.contains(Scope::ApprovalsRead) {
        let can_approve_mail =
            ctx.scopes.contains(Scope::MailRead) && ctx.scopes.contains(Scope::MailApprove);
        queues.push(queue(
            "approvals_pending",
            repo::actionable_pending_approvals_queue(&mut tx, ctx.principal_id, can_approve_mail)
                .await?,
        ));
    }

    if ctx.scopes.contains(Scope::BookingRead) {
        let timezone = repo::workspace_timezone(&mut tx).await?;
        let timezone: Tz = timezone
            .parse()
            .map_err(|_| AppError::internal("workspace timezone is invalid"))?;
        let today = Utc::now().with_timezone(&timezone).date_naive();
        let tomorrow = today
            .succ_opt()
            .ok_or_else(|| AppError::internal("workspace local date is out of range"))?;
        let day_start = first_instant_of_local_day(today, timezone)
            .ok_or_else(|| AppError::internal("workspace local day has no start instant"))?;
        let next_day_start = first_instant_of_local_day(tomorrow, timezone)
            .ok_or_else(|| AppError::internal("next workspace local day has no start instant"))?;
        let host_scope = (!can_view_team || mine).then_some(ctx.principal_id);
        queues.push(queue(
            "meetings_today",
            repo::today_bookings_queue(&mut tx, host_scope, day_start, next_day_start).await?,
        ));
    }

    let own_jobs = mine.then_some(ctx.principal_id);
    queues.push(queue(
        "dead_jobs",
        repo::dead_jobs_queue(&mut tx, own_jobs).await?,
    ));
    if can_view_team && scope == "team" {
        queues.push(queue(
            "outbox_backlog",
            repo::outbox_backlog_queue(&mut tx).await?,
        ));
    }
    queues.sort_by_key(|queue| match queue.key.as_str() {
        "inbox_unhandled" => 0,
        "approvals_pending" => 1,
        "meetings_today" => 2,
        "outbound_unknown" => 3,
        "sync_lag" => 4,
        "reauth_required" => 5,
        "dead_jobs" => 6,
        "outbox_backlog" => 7,
        _ => 8,
    });
    tx.commit().await?;

    Ok(DashboardSummaryResponse {
        period: DashboardPeriod { from, to },
        generated_at: Utc::now(),
        scope: scope.into(),
        queues,
        kpis: DashboardKpis {
            eligible_contacted_companies: eligible,
            reply_company_ratio: ratio(replies, eligible),
            meeting_conversion: ratio(meetings, eligible),
            first_reply: MedianKpi {
                sample_size,
                median_seconds,
            },
            agent_proposal_adoption: ratio(adopted, reviewed),
        },
        operational: OperationalCounts {
            unknown_outbound_messages: counts.unknown_outbound_messages,
            unknown_jobs: counts.unknown_jobs,
            dead_jobs: counts.dead_jobs,
            outbox_backlog: counts.outbox_backlog,
            reauth_required_mailboxes: counts.reauth_required_mailboxes,
            emergency_stop: counts.emergency_stop,
            send_workers_locked: counts.send_workers_locked,
        },
    })
}
