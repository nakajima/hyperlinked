use axum::http::StatusCode;
use hyperlinked_macros::params;
use sea_orm::EntityTrait;

use crate::{
    app::{
        controllers::flash::FlashName,
        helpers::hyperlinks::{
            latest_job_optional, load_og_summary, select_show_display_title, show_artifact_kinds,
            show_path, to_response,
        },
        models::{hyperlink::HyperlinkInput, settings},
    },
    entity::{
        hyperlink::{self},
        hyperlink_artifact::HyperlinkArtifactKind,
    },
    server::hyperlink_fetcher::{HyperlinkFetchQuery, HyperlinkFetcher},
};

use super::{
    HyperlinkLookupResponse, HyperlinksIndexQueryResponse, HyperlinksIndexResponse,
    SHOW_TIMELINE_LIMIT, render_edit, render_edit_form, render_index, render_new, render_new_form,
    render_show,
};
use super::{
    params::HyperlinkPathId,
    result::{ActionResult, ControllerContext, RequestFormat},
};

#[params(pub(crate) IndexParams {
    q: Option<String>,
    page: Option<u64>,
})]
pub(crate) async fn index(ctx: ControllerContext, params: IndexParams) -> ActionResult {
    let query = HyperlinkFetchQuery {
        q: params.q.clone(),
        page: params.page,
    };
    let results = match HyperlinkFetcher::new(&ctx.state().connection, query)
        .fetch()
        .await
    {
        Ok(results) => results,
        Err(err) => return ctx.internal_error(format!("failed to list hyperlinks: {err}")),
    };

    match ctx.format() {
        RequestFormat::Html => ctx.page("Hyperlinks", render_index(&results)),
        RequestFormat::Json => {
            let items = results
                .links
                .iter()
                .map(|link| to_response(link, results.latest_jobs.get(&link.id)))
                .collect::<Vec<_>>();
            ctx.json(
                StatusCode::OK,
                HyperlinksIndexResponse {
                    items,
                    query: HyperlinksIndexQueryResponse {
                        raw_q: results.raw_q,
                        parsed: results.parsed_query,
                        ignored_tokens: results.ignored_tokens,
                        free_text: results.free_text,
                    },
                },
            )
        }
    }
}

pub(crate) async fn new(ctx: ControllerContext) -> ActionResult {
    ctx.page("New Hyperlink", render_new())
}

#[params(pub(crate) LookupParams {
    url: Option<String>,
})]
pub(crate) async fn lookup(ctx: ControllerContext, params: LookupParams) -> ActionResult {
    let Some(url) = params.url.as_deref() else {
        return ctx.json(
            StatusCode::OK,
            HyperlinkLookupResponse {
                status: "invalid_url".to_string(),
                id: None,
                canonical_url: None,
            },
        );
    };

    let canonicalized = match crate::app::models::url_canonicalize::canonicalize_submitted_url(url)
    {
        Ok(canonicalized) => canonicalized,
        Err(_) => {
            return ctx.json(
                StatusCode::OK,
                HyperlinkLookupResponse {
                    status: "invalid_url".to_string(),
                    id: None,
                    canonical_url: None,
                },
            );
        }
    };

    match crate::app::models::hyperlink::find_by_url(
        &ctx.state().connection,
        &canonicalized.canonical_url,
    )
    .await
    {
        Ok(Some(link)) => {
            let status =
                if link.discovery_depth == crate::app::models::hyperlink::ROOT_DISCOVERY_DEPTH {
                    "root"
                } else {
                    "discovered"
                };
            ctx.json(
                StatusCode::OK,
                HyperlinkLookupResponse {
                    status: status.to_string(),
                    id: Some(link.id),
                    canonical_url: Some(canonicalized.canonical_url),
                },
            )
        }
        Ok(None) => ctx.json(
            StatusCode::OK,
            HyperlinkLookupResponse {
                status: "not_found".to_string(),
                id: None,
                canonical_url: Some(canonicalized.canonical_url),
            },
        ),
        Err(err) => ctx.internal_error(format!("failed to lookup hyperlink by url: {err}")),
    }
}

#[params(pub(crate) CreateParams {
    title: String,
    url: String,
})]
pub(crate) async fn create(ctx: ControllerContext, params: CreateParams) -> ActionResult {
    let submitted_title = params.title.clone();
    let submitted_url = params.url.clone();
    let input = match crate::app::models::hyperlink::validate_and_normalize(HyperlinkInput {
        title: params.title,
        url: params.url,
    })
    .await
    {
        Ok(input) => input,
        Err(msg) => {
            return match ctx.format() {
                RequestFormat::Html => ctx.page_with_status(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "New Hyperlink",
                    render_new_form(&submitted_title, &submitted_url, &[msg]),
                ),
                RequestFormat::Json => ctx.bad_request(msg),
            };
        }
    };

    match crate::app::models::hyperlink::insert(
        &ctx.state().connection,
        input,
        ctx.state().processing_queue.as_ref(),
    )
    .await
    {
        Ok(link) => {
            let latest_job = latest_job_optional(ctx.state(), link.id).await;
            match ctx.format() {
                RequestFormat::Html => {
                    ctx.redirect_with_flash(show_path(link.id), FlashName::Notice, "Saved link.")
                }
                RequestFormat::Json => {
                    ctx.json(StatusCode::CREATED, to_response(&link, latest_job.as_ref()))
                }
            }
        }
        Err(err) => ctx.internal_error(format!("failed to create hyperlink: {err}")),
    }
}

#[params(pub(crate) ShowParams {
    id: HyperlinkPathId,
})]
pub(crate) async fn show(ctx: ControllerContext, params: ShowParams) -> ActionResult {
    let id = params.id.get();
    match hyperlink::Entity::find_by_id(id)
        .one(&ctx.state().connection)
        .await
    {
        Ok(Some(link)) => {
            let latest_job =
                match crate::app::models::hyperlink_processing_job::latest_for_hyperlink(
                    &ctx.state().connection,
                    id,
                )
                .await
                {
                    Ok(job) => job,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load processing job for hyperlink {id}: {err}"
                        ));
                    }
                };

            let latest_artifacts =
                match crate::app::models::hyperlink_artifact::latest_for_hyperlink_kinds(
                    &ctx.state().connection,
                    id,
                    &show_artifact_kinds(),
                )
                .await
                {
                    Ok(artifacts) => artifacts,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load artifacts for hyperlink {id}: {err}"
                        ));
                    }
                };

            let recent_jobs =
                match crate::app::models::hyperlink_processing_job::recent_for_hyperlink(
                    &ctx.state().connection,
                    id,
                    SHOW_TIMELINE_LIMIT,
                )
                .await
                {
                    Ok(jobs) => jobs,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load job history for hyperlink {id}: {err}"
                        ));
                    }
                };

            let discovered_links =
                match crate::app::models::hyperlink_relation::children_for_parent(
                    &ctx.state().connection,
                    id,
                )
                .await
                {
                    Ok(children) => children,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load discovered links for hyperlink {id}: {err}"
                        ));
                    }
                };
            let discovered_link_ids = discovered_links
                .iter()
                .map(|link| link.id)
                .collect::<Vec<_>>();
            let discovered_latest_jobs =
                match crate::app::models::hyperlink_processing_job::latest_for_hyperlinks(
                    &ctx.state().connection,
                    &discovered_link_ids,
                )
                .await
                {
                    Ok(jobs) => jobs,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load discovered link processing jobs for hyperlink {id}: {err}"
                        ));
                    }
                };
            let discovered_thumbnail_artifacts =
                match crate::app::models::hyperlink_artifact::latest_for_hyperlinks_kind(
                    &ctx.state().connection,
                    &discovered_link_ids,
                    HyperlinkArtifactKind::ScreenshotThumbWebp,
                )
                .await
                {
                    Ok(artifacts) => artifacts,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load discovered link thumbnails for hyperlink {id}: {err}"
                        ));
                    }
                };
            let discovered_dark_thumbnail_artifacts =
                match crate::app::models::hyperlink_artifact::latest_for_hyperlinks_kind(
                    &ctx.state().connection,
                    &discovered_link_ids,
                    HyperlinkArtifactKind::ScreenshotThumbDarkWebp,
                )
                .await
                {
                    Ok(artifacts) => artifacts,
                    Err(err) => {
                        return ctx.internal_error(format!(
                            "failed to load discovered link dark thumbnails for hyperlink {id}: {err}"
                        ));
                    }
                };
            let artifact_settings = match settings::load(&ctx.state().connection).await {
                Ok(settings) => settings,
                Err(err) => {
                    return ctx.internal_error(format!(
                        "failed to load artifact settings for hyperlink {id}: {err}"
                    ));
                }
            };

            match ctx.format() {
                RequestFormat::Html => {
                    let og_summary = load_og_summary(&link);
                    let display_title = select_show_display_title(&link, og_summary.as_ref());
                    ctx.page(
                        "Show Hyperlink",
                        render_show(
                            &link,
                            latest_job.as_ref(),
                            &latest_artifacts,
                            &recent_jobs,
                            &discovered_links,
                            &discovered_latest_jobs,
                            &discovered_thumbnail_artifacts,
                            &discovered_dark_thumbnail_artifacts,
                            artifact_settings,
                            display_title,
                            og_summary,
                        ),
                    )
                }
                RequestFormat::Json => {
                    ctx.json(StatusCode::OK, to_response(&link, latest_job.as_ref()))
                }
            }
        }
        Ok(None) => ctx.error(StatusCode::NOT_FOUND, format!("hyperlink {id} not found")),
        Err(err) => ctx.internal_error(format!("failed to fetch hyperlink {id}: {err}")),
    }
}

#[params(pub(crate) VisitParams { id: i32 })]
pub(crate) async fn visit(ctx: ControllerContext, params: VisitParams) -> ActionResult {
    match crate::app::models::hyperlink::increment_click_count_by_id(
        &ctx.state().connection,
        params.id,
    )
    .await
    {
        Ok(Some(link)) => ctx.temporary_redirect(link.url),
        Ok(None) => ctx.error(
            StatusCode::NOT_FOUND,
            format!("hyperlink {} not found", params.id),
        ),
        Err(err) => ctx.internal_error(format!("failed to visit hyperlink {}: {err}", params.id)),
    }
}

#[params(pub(crate) ClickParams { id: i32 })]
pub(crate) async fn click(ctx: ControllerContext, params: ClickParams) -> ActionResult {
    match crate::app::models::hyperlink::increment_click_count_by_id(
        &ctx.state().connection,
        params.id,
    )
    .await
    {
        Ok(Some(_)) => ctx.no_content(),
        Ok(None) => ctx.error(
            StatusCode::NOT_FOUND,
            format!("hyperlink {} not found", params.id),
        ),
        Err(err) => ctx.internal_error(format!(
            "failed to track click for hyperlink {}: {err}",
            params.id
        )),
    }
}

#[params(pub(crate) EditParams { id: i32 })]
pub(crate) async fn edit(ctx: ControllerContext, params: EditParams) -> ActionResult {
    match hyperlink::Entity::find_by_id(params.id)
        .one(&ctx.state().connection)
        .await
    {
        Ok(Some(link)) => ctx.page("Edit Hyperlink", render_edit(&link)),
        Ok(None) => ctx.error(
            StatusCode::NOT_FOUND,
            format!("hyperlink {} not found", params.id),
        ),
        Err(err) => ctx.internal_error(format!("failed to fetch hyperlink {}: {err}", params.id)),
    }
}

#[params(pub(crate) UpdateParams {
    id: HyperlinkPathId,
    title: String,
    url: String,
})]
pub(crate) async fn update(ctx: ControllerContext, params: UpdateParams) -> ActionResult {
    let id = params.id.get();
    let submitted_title = params.title.clone();
    let submitted_url = params.url.clone();
    let input = match crate::app::models::hyperlink::validate_and_normalize(HyperlinkInput {
        title: params.title,
        url: params.url,
    })
    .await
    {
        Ok(input) => input,
        Err(msg) => {
            return match ctx.format() {
                RequestFormat::Html => ctx.page_with_status(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Edit Hyperlink",
                    render_edit_form(id, &submitted_title, &submitted_url, &[msg]),
                ),
                RequestFormat::Json => ctx.bad_request(msg),
            };
        }
    };

    match crate::app::models::hyperlink::update_by_id(
        &ctx.state().connection,
        id,
        input,
        ctx.state().processing_queue.as_ref(),
    )
    .await
    {
        Ok(Some(link)) => {
            let latest_job = latest_job_optional(ctx.state(), link.id).await;
            match ctx.format() {
                RequestFormat::Html => {
                    ctx.redirect_with_flash(show_path(link.id), FlashName::Notice, "Updated link.")
                }
                RequestFormat::Json => {
                    ctx.json(StatusCode::OK, to_response(&link, latest_job.as_ref()))
                }
            }
        }
        Ok(None) => ctx.error(StatusCode::NOT_FOUND, format!("hyperlink {id} not found")),
        Err(err) => ctx.internal_error(format!("failed to update hyperlink {id}: {err}")),
    }
}

#[params(pub(crate) DestroyParams { id: HyperlinkPathId })]
pub(crate) async fn destroy(ctx: ControllerContext, params: DestroyParams) -> ActionResult {
    let id = params.id.get();
    if matches!(ctx.format(), RequestFormat::Json) {
        return ctx.error(
            StatusCode::NOT_FOUND,
            "delete json endpoint is not supported",
        );
    }

    match crate::app::models::hyperlink::delete_by_id_with_tombstone(&ctx.state().connection, id)
        .await
    {
        Ok(false) => ctx.error(StatusCode::NOT_FOUND, format!("hyperlink {id} not found")),
        Ok(true) => ctx.redirect_with_flash("/hyperlinks", FlashName::Notice, "Deleted link."),
        Err(err) => ctx.internal_error(format!("failed to delete hyperlink {id}: {err}")),
    }
}

#[params(pub(crate) ReprocessParams { id: i32 })]
pub(crate) async fn reprocess(ctx: ControllerContext, params: ReprocessParams) -> ActionResult {
    match crate::app::models::hyperlink::enqueue_reprocess_by_id(
        &ctx.state().connection,
        params.id,
        ctx.state().processing_queue.as_ref(),
    )
    .await
    {
        Ok(Some((link, true))) => ctx.redirect_with_flash(
            show_path(link.id),
            FlashName::Notice,
            "Queued reprocessing.",
        ),
        Ok(Some((link, false))) => ctx.redirect_with_flash(
            show_path(link.id),
            FlashName::Notice,
            "Reprocessing skipped because source artifact collection is disabled.",
        ),
        Ok(None) => ctx.error(
            StatusCode::NOT_FOUND,
            format!("hyperlink {} not found", params.id),
        ),
        Err(err) => ctx.internal_error(format!(
            "failed to enqueue for processing hyperlink {}: {err}",
            params.id
        )),
    }
}
