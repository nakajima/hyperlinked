use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::{
    app::{
        controllers::flash::FlashName,
        helpers::hyperlinks::{
            artifact_download_file_extension, artifact_fetch_dependency_label,
            artifact_inline_path, artifact_kind_label, artifact_kind_slug,
            is_readability_artifact_kind, parse_artifact_kind, show_path,
        },
        models::{
            artifact_job::{self, ArtifactFetchMode, ArtifactJobResolveResult},
            hyperlink_search_doc,
        },
    },
    entity::{hyperlink, hyperlink_artifact, hyperlink_artifact::HyperlinkArtifactKind},
};

use super::result::{ActionResult, ControllerContext};
use hyperlinked_macros::params as params_attr;

#[params_attr(pub(crate) ArtifactParams {
    id: i32,
    kind: String,
})]
pub(crate) async fn download_latest_artifact(
    ctx: ControllerContext,
    params: ArtifactParams,
) -> ActionResult {
    serve_latest_artifact(ctx, params.id, params.kind, true).await
}

#[params_attr(pub(crate) ArtifactParamsInline {
    id: i32,
    kind: String,
})]
pub(crate) async fn render_latest_artifact_inline(
    ctx: ControllerContext,
    params: ArtifactParamsInline,
) -> ActionResult {
    serve_latest_artifact(ctx, params.id, params.kind, false).await
}

#[params_attr(pub(crate) PdfPreviewParams { id: i32 })]
pub(crate) async fn render_pdf_source_preview(
    ctx: ControllerContext,
    params: PdfPreviewParams,
) -> ActionResult {
    match hyperlink::Entity::find_by_id(params.id)
        .one(&ctx.state().connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ctx.error(
                StatusCode::NOT_FOUND,
                format!("hyperlink {} not found", params.id),
            );
        }
        Err(err) => {
            return ctx.internal_error(format!("failed to fetch hyperlink {}: {err}", params.id));
        }
    }

    match crate::app::models::hyperlink_artifact::latest_for_hyperlink_kind(
        &ctx.state().connection,
        params.id,
        HyperlinkArtifactKind::PdfSource,
    )
    .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ctx.error(
                StatusCode::NOT_FOUND,
                format!(
                    "no pdf_source artifact available for hyperlink {}",
                    params.id
                ),
            );
        }
        Err(err) => {
            return ctx.internal_error(format!(
                "failed to load artifact for hyperlink {}: {err}",
                params.id
            ));
        }
    }

    ctx.binary(
        StatusCode::OK,
        HeaderMap::from_iter([(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        )]),
        format!(
            r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <style>
      html, body {{
        margin: 0;
        width: 100%;
        height: 100%;
        background: #fff;
        color-scheme: light;
      }}
      embed {{
        display: block;
        width: 100%;
        height: 100%;
        background: #fff;
      }}
    </style>
  </head>
  <body>
    <embed src="{}#zoom=page-width" type="application/pdf">
  </body>
</html>
"#,
            artifact_inline_path(params.id, &HyperlinkArtifactKind::PdfSource)
        )
        .into_bytes(),
    )
}

#[params_attr(pub(crate) ArtifactMutationParams {
    id: i32,
    kind: String,
})]
pub(crate) async fn delete_artifact_kind(
    ctx: ControllerContext,
    params: ArtifactMutationParams,
) -> ActionResult {
    let Some(kind) = parse_artifact_kind(&params.kind) else {
        return ctx.error(StatusCode::BAD_REQUEST, "invalid artifact kind");
    };

    match hyperlink::Entity::find_by_id(params.id)
        .one(&ctx.state().connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ctx.error(
                StatusCode::NOT_FOUND,
                format!("hyperlink {} not found", params.id),
            );
        }
        Err(err) => {
            return ctx.internal_error(format!("failed to fetch hyperlink {}: {err}", params.id));
        }
    }

    let delete_result = match hyperlink_artifact::Entity::delete_many()
        .filter(hyperlink_artifact::Column::HyperlinkId.eq(params.id))
        .filter(hyperlink_artifact::Column::Kind.eq(kind.clone()))
        .exec(&ctx.state().connection)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            return ctx.internal_error(format!(
                "failed to delete artifacts for hyperlink {}: {err}",
                params.id
            ));
        }
    };

    if is_readability_artifact_kind(&kind)
        && let Err(error) = hyperlink_search_doc::clear_readable_text_for_hyperlink(
            &ctx.state().connection,
            params.id,
        )
        .await
    {
        if !hyperlink_search_doc::is_search_doc_missing_error(&error) {
            return ctx.internal_error(format!(
                "failed to clear readability search text for hyperlink {}: {error}",
                params.id
            ));
        }
    }

    let label = artifact_kind_label(&kind);
    let message = if delete_result.rows_affected > 0 {
        format!("Deleted {label} artifact(s).")
    } else {
        format!("No {label} artifacts to delete.")
    };

    ctx.redirect_with_flash(show_path(params.id), FlashName::Notice, message)
}

#[params_attr(pub(crate) ArtifactFetchParams {
    id: i32,
    kind: String,
})]
pub(crate) async fn fetch_artifact_kind(
    ctx: ControllerContext,
    params: ArtifactFetchParams,
) -> ActionResult {
    let Some(artifact_kind) = parse_artifact_kind(&params.kind) else {
        return ctx.error(StatusCode::BAD_REQUEST, "invalid artifact kind");
    };

    match hyperlink::Entity::find_by_id(params.id)
        .one(&ctx.state().connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ctx.error(
                StatusCode::NOT_FOUND,
                format!("hyperlink {} not found", params.id),
            );
        }
        Err(err) => {
            return ctx.internal_error(format!("failed to fetch hyperlink {}: {err}", params.id));
        }
    }

    let Some(queue) = ctx.state().processing_queue.as_ref() else {
        return ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Alert,
            "Queue workers are unavailable in this environment.",
        );
    };

    let result = match artifact_job::resolve_and_enqueue_for_artifact_kind(
        &ctx.state().connection,
        params.id,
        artifact_kind.clone(),
        ArtifactFetchMode::RefetchTarget,
        Some(queue),
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            return ctx.internal_error(format!(
                "failed to enqueue artifact fetch for hyperlink {}: {err}",
                params.id
            ));
        }
    };

    match result {
        ArtifactJobResolveResult::EnqueuedRequested { .. } => ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Notice,
            format!("Queued fetch for {}.", artifact_kind_label(&artifact_kind)),
        ),
        ArtifactJobResolveResult::EnqueuedDependency {
            dependency_kind, ..
        } => ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Notice,
            format!(
                "Queued {} first to satisfy dependencies for {}.",
                artifact_fetch_dependency_label(&dependency_kind),
                artifact_kind_label(&artifact_kind)
            ),
        ),
        ArtifactJobResolveResult::DisabledRequested { .. } => ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Alert,
            format!(
                "Fetching {} is disabled by artifact settings.",
                artifact_kind_label(&artifact_kind)
            ),
        ),
        ArtifactJobResolveResult::DisabledDependency {
            dependency_kind, ..
        } => ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Alert,
            format!(
                "Cannot fetch {} because {} is disabled by artifact settings.",
                artifact_kind_label(&artifact_kind),
                artifact_fetch_dependency_label(&dependency_kind)
            ),
        ),
        ArtifactJobResolveResult::UnfetchableDependency {
            dependency_kind, ..
        } => ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Alert,
            format!(
                "Cannot fetch {} because {} cannot be fetched from this hyperlink URL.",
                artifact_kind_label(&artifact_kind),
                artifact_fetch_dependency_label(&dependency_kind)
            ),
        ),
        ArtifactJobResolveResult::AlreadySatisfied { .. } => ctx.redirect_with_flash(
            show_path(params.id),
            FlashName::Notice,
            format!(
                "{} is already available; no fetch was queued.",
                artifact_kind_label(&artifact_kind)
            ),
        ),
        ArtifactJobResolveResult::UnsupportedArtifactKind { .. }
        | ArtifactJobResolveResult::UnsupportedJobKind { .. } => {
            ctx.error(StatusCode::BAD_REQUEST, "unsupported artifact fetch kind")
        }
    }
}

async fn serve_latest_artifact(
    ctx: ControllerContext,
    id: i32,
    kind: String,
    as_attachment: bool,
) -> ActionResult {
    let Some(kind) = parse_artifact_kind(&kind) else {
        return ctx.error(StatusCode::BAD_REQUEST, "invalid artifact kind");
    };

    match hyperlink::Entity::find_by_id(id)
        .one(&ctx.state().connection)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return ctx.error(StatusCode::NOT_FOUND, format!("hyperlink {id} not found")),
        Err(err) => return ctx.internal_error(format!("failed to fetch hyperlink {id}: {err}")),
    }

    let artifact = match crate::app::models::hyperlink_artifact::latest_for_hyperlink_kind(
        &ctx.state().connection,
        id,
        kind.clone(),
    )
    .await
    {
        Ok(Some(artifact)) => artifact,
        Ok(None) => {
            return ctx.error(
                StatusCode::NOT_FOUND,
                format!(
                    "no {} artifact available for hyperlink {id}",
                    artifact_kind_slug(&kind)
                ),
            );
        }
        Err(err) => {
            return ctx
                .internal_error(format!("failed to load artifact for hyperlink {id}: {err}"));
        }
    };

    let payload = match crate::app::models::hyperlink_artifact::load_payload(&artifact).await {
        Ok(payload) => payload,
        Err(err) => {
            return ctx.internal_error(format!(
                "failed to read artifact payload for hyperlink {id}: {err}"
            ));
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&artifact.content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );

    if as_attachment {
        let extension = artifact_download_file_extension(&kind, &artifact);
        let filename = format!("hyperlink-{id}-{}.{}", artifact_kind_slug(&kind), extension);
        if let Ok(disposition) =
            HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        {
            headers.insert(header::CONTENT_DISPOSITION, disposition);
        }
    }

    ctx.binary(StatusCode::OK, headers, payload)
}
