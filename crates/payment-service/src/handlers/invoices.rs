//! Fatura uçları (iç API, `X-Internal-Token`): üyenin faturaları, PDF, yönetici işlemleri.
//! `member_id` her zaman Next.js sunucusunun oturumundan gelir.

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::einvoice::{repo, turkcell::Client, DocType};
use crate::error::AppError;
use crate::handlers::admin_members::{audit, Actor};
use crate::AppState;

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Fatura bulunamadı" }))).into_response()
}

/// GET /api/v1/members/:member_id/invoices — en yeni önce.
pub async fn list_for_member(State(state): State<AppState>, Path(member_id): Path<i32>) -> Result<Json<Value>, AppError> {
    let items = repo::member_invoices(&state.db, member_id).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct PdfQuery {
    /// Üye isteğinde zorunlu (sahiplik); yönetici isteğinde yok.
    member_id: Option<i32>,
}

/// GET /api/v1/invoices/:id/pdf?member_id= — Turkcell'den PDF (aracı).
pub async fn pdf(State(state): State<AppState>, Path(id): Path<i32>, Query(q): Query<PdfQuery>) -> Result<Response, AppError> {
    let Some((owner, doc, ettn, number)) = repo::pdf_ref(&state.db, id).await? else { return Ok(not_found()) };
    if q.member_id.is_some_and(|m| m != owner) {
        return Ok(not_found());
    }
    let (Some(cfg), Some(doc)) = (state.config.einvoice.as_ref(), DocType::parse(&doc)) else {
        return Ok((StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": "e-Fatura servisi kapalı" }))).into_response());
    };
    let tc = Client { http: &state.http, base: &cfg.base_url, key: &cfg.api_key };
    match tc.pdf(doc, &ettn).await {
        Ok(bytes) => {
            let name = format!("{}.pdf", number.unwrap_or_else(|| ettn.clone()));
            Ok((
                [
                    (header::CONTENT_TYPE, "application/pdf".to_string()),
                    (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
                    (header::CACHE_CONTROL, "private, no-store".to_string()),
                ],
                bytes,
            )
                .into_response())
        }
        Err(e) => {
            tracing::warn!(invoice = id, "Fatura PDF'i alınamadı: {e}");
            Ok((StatusCode::BAD_GATEWAY, Json(json!({ "error": "Fatura şu anda indirilemiyor, biraz sonra deneyin" }))).into_response())
        }
    }
}

#[derive(Deserialize)]
pub struct AdminAction {
    actor: Actor,
    #[serde(default)]
    note: Option<String>,
    /// Yalnızca "elle kesildi": entegratör dışında kesilen faturanın numarası.
    #[serde(default)]
    invoice_no: Option<String>,
}

async fn admin_change(state: &AppState, id: i32, req: AdminAction, manual: bool) -> Result<Response, AppError> {
    let mut tx = state.db.begin().await.map_err(anyhow::Error::from)?;
    let Some(before) = repo::state(&mut *tx, id).await? else { return Ok(not_found()) };
    let number = req.invoice_no.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if number.is_some_and(|n| n.len() > 50) {
        return Err(AppError::BadRequest("Fatura numarası en çok 50 karakter".into()));
    }
    let changed = if manual { repo::mark_manual(&mut *tx, id, number).await? } else { repo::requeue(&mut *tx, id).await? };
    if !changed {
        return Err(AppError::BadRequest(format!("Fatura bu durumda değiştirilemez ({})", before.status)));
    }
    let action = if manual { "invoice.manual" } else { "invoice.retry" };
    let after = json!({ "status": if manual { "manual" } else { "pending" }, "invoice_no": number });
    audit(&mut *tx, &req.actor, before.member_id, action, &json!({ "invoice_id": id, "status": before.status, "error": before.last_error }), &after, req.note.as_deref())
        .await?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    tracing::info!(invoice = id, actor = req.actor.actor_id, action, "Yönetici fatura işlemi");
    Ok(Json(json!({ "ok": true })).into_response())
}

/// POST /api/v1/admin/invoices/:id/retry — hatalı faturayı yeniden kuyruğa alır.
pub async fn retry(State(state): State<AppState>, Path(id): Path<i32>, Json(req): Json<AdminAction>) -> Result<Response, AppError> {
    admin_change(&state, id, req, false).await
}

/// POST /api/v1/admin/invoices/:id/manual — entegratör dışında kesildi olarak kapatır.
pub async fn manual(State(state): State<AppState>, Path(id): Path<i32>, Json(req): Json<AdminAction>) -> Result<Response, AppError> {
    admin_change(&state, id, req, true).await
}
