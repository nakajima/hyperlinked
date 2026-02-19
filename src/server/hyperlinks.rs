use axum::{
    Json, Router, body,
    extract::{Form, Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing,
};
use maud::{Markup, html};
use sea_orm::{EntityTrait, QueryOrder};
use serde::{Deserialize, Serialize};

use crate::{
    entity::hyperlink::{self, HyperlinkProcessingState},
    model::hyperlink::HyperlinkInput,
    server::context::Context,
};

use super::html_layout;

pub fn links() -> Router<Context> {
    Router::new()
        .route("/hyperlinks", routing::get(index))
        .route("/hyperlinks/new", routing::get(new))
        .route("/hyperlinks.json", routing::get(index_json))
        .route("/hyperlinks", routing::post(create))
        .route("/hyperlinks.json", routing::post(create_json))
        .route("/hyperlinks/{id}/click", routing::post(click))
        .route("/hyperlinks/{id}/visit", routing::get(visit))
        .route("/hyperlinks/{id}/edit", routing::get(edit))
        .route("/hyperlinks/{id}/update", routing::post(update_html_post))
        .route("/hyperlinks/{id}/delete", routing::post(delete_html_post))
        .route("/hyperlinks/{id_or_ext}", routing::get(show_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::patch(update_by_path))
        .route("/hyperlinks/{id_or_ext}", routing::delete(delete_by_path))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HyperlinkResponse {
    id: i32,
    title: String,
    url: String,
    clicks_count: i32,
    last_clicked_at: Option<String>,
    processing_state: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Clone, Debug, Serialize)]
struct DeleteResponse {
    id: i32,
    deleted: bool,
}

#[derive(Clone, Copy, Debug)]
enum ResponseKind {
    Text,
    Json,
}

async fn index(State(state): State<Context>) -> Response {
    index_with_kind(&state, ResponseKind::Text).await
}

async fn new() -> Response {
    html_page("New Hyperlink", render_new()).into_response()
}

async fn index_json(State(state): State<Context>) -> Response {
    index_with_kind(&state, ResponseKind::Json).await
}

async fn create(State(state): State<Context>, Form(input): Form<HyperlinkInput>) -> Response {
    create_with_kind(&state, input, ResponseKind::Text).await
}

async fn create_json(State(state): State<Context>, Json(input): Json<HyperlinkInput>) -> Response {
    create_with_kind(&state, input, ResponseKind::Json).await
}

async fn show_by_path(Path(id_or_ext): Path<String>, State(state): State<Context>) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    show_with_kind(&state, id, kind).await
}

async fn update_by_path(
    Path(id_or_ext): Path<String>,
    State(state): State<Context>,
    request: Request,
) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    let input = match parse_update_input(kind, request).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    update_with_kind(&state, id, input, kind).await
}

async fn update_html_post(
    Path(id): Path<i32>,
    State(state): State<Context>,
    Form(input): Form<HyperlinkInput>,
) -> Response {
    update_with_kind(&state, id, input, ResponseKind::Text).await
}

async fn delete_by_path(Path(id_or_ext): Path<String>, State(state): State<Context>) -> Response {
    let (id, kind) = match parse_id_and_kind(&id_or_ext) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    if matches!(kind, ResponseKind::Json) {
        return response_error(
            kind,
            StatusCode::NOT_FOUND,
            "delete json endpoint is not supported",
        );
    }
    delete_with_kind(&state, id, kind).await
}

async fn delete_html_post(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    delete_with_kind(&state, id, ResponseKind::Text).await
}

async fn index_with_kind(state: &Context, kind: ResponseKind) -> Response {
    match hyperlink::Entity::find()
        .order_by_desc(hyperlink::Column::CreatedAt)
        .all(&state.connection)
        .await
    {
        Ok(links) => match kind {
            ResponseKind::Text => html_page("Hyperlinks", render_index(&links)).into_response(),
            ResponseKind::Json => {
                let response = links.iter().map(to_response).collect::<Vec<_>>();
                (StatusCode::OK, Json(response)).into_response()
            }
        },
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to list hyperlinks: {err}"),
        ),
    }
}

async fn create_with_kind(state: &Context, input: HyperlinkInput, kind: ResponseKind) -> Response {
    let input = match crate::model::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::model::hyperlink::insert(&state.connection, input, state.processing_queue.as_ref())
        .await
    {
        Ok(link) => match kind {
            ResponseKind::Text => Redirect::to(&show_path(link.id)).into_response(),
            ResponseKind::Json => (StatusCode::CREATED, Json(to_response(&link))).into_response(),
        },
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create hyperlink: {err}"),
        ),
    }
}

async fn show_with_kind(state: &Context, id: i32, kind: ResponseKind) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(link)) => match kind {
            ResponseKind::Text => html_page("Show Hyperlink", render_show(&link)).into_response(),
            ResponseKind::Json => (StatusCode::OK, Json(to_response(&link))).into_response(),
        },
        Ok(None) => response_error(
            kind,
            StatusCode::NOT_FOUND,
            format!("hyperlink {id} not found"),
        ),
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to fetch hyperlink {id}: {err}"),
        ),
    }
}

async fn visit(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match crate::model::hyperlink::increment_click_count_by_id(&state.connection, id).await {
        Ok(Some(link)) => Redirect::temporary(&link.url).into_response(),
        Ok(None) => response_error(
            ResponseKind::Text,
            StatusCode::NOT_FOUND,
            format!("hyperlink {id} not found"),
        ),
        Err(err) => response_error(
            ResponseKind::Text,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to visit hyperlink {id}: {err}"),
        ),
    }
}

async fn click(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match crate::model::hyperlink::increment_click_count_by_id(&state.connection, id).await {
        Ok(Some(_)) => StatusCode::NO_CONTENT.into_response(),
        Ok(None) => response_error(
            ResponseKind::Text,
            StatusCode::NOT_FOUND,
            format!("hyperlink {id} not found"),
        ),
        Err(err) => response_error(
            ResponseKind::Text,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to track click for hyperlink {id}: {err}"),
        ),
    }
}

async fn edit(Path(id): Path<i32>, State(state): State<Context>) -> Response {
    match hyperlink::Entity::find_by_id(id)
        .one(&state.connection)
        .await
    {
        Ok(Some(link)) => html_page("Edit Hyperlink", render_edit(&link)).into_response(),
        Ok(None) => response_error(
            ResponseKind::Text,
            StatusCode::NOT_FOUND,
            format!("hyperlink {id} not found"),
        ),
        Err(err) => response_error(
            ResponseKind::Text,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to fetch hyperlink {id}: {err}"),
        ),
    }
}

async fn update_with_kind(
    state: &Context,
    id: i32,
    input: HyperlinkInput,
    kind: ResponseKind,
) -> Response {
    let input = match crate::model::hyperlink::validate_and_normalize(input).await {
        Ok(input) => input,
        Err(msg) => return response_error(kind, StatusCode::BAD_REQUEST, msg),
    };

    match crate::model::hyperlink::update_by_id(
        &state.connection,
        id,
        input,
        state.processing_queue.as_ref(),
    )
    .await
    {
        Ok(Some(link)) => match kind {
            ResponseKind::Text => Redirect::to(&show_path(link.id)).into_response(),
            ResponseKind::Json => (StatusCode::OK, Json(to_response(&link))).into_response(),
        },
        Ok(None) => response_error(
            kind,
            StatusCode::NOT_FOUND,
            format!("hyperlink {id} not found"),
        ),
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to update hyperlink {id}: {err}"),
        ),
    }
}

async fn delete_with_kind(state: &Context, id: i32, kind: ResponseKind) -> Response {
    match hyperlink::Entity::delete_by_id(id)
        .exec(&state.connection)
        .await
    {
        Ok(result) if result.rows_affected == 0 => response_error(
            kind,
            StatusCode::NOT_FOUND,
            format!("hyperlink {id} not found"),
        ),
        Ok(_) => match kind {
            ResponseKind::Text => Redirect::to("/hyperlinks").into_response(),
            ResponseKind::Json => {
                (StatusCode::OK, Json(DeleteResponse { id, deleted: true })).into_response()
            }
        },
        Err(err) => response_error(
            kind,
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to delete hyperlink {id}: {err}"),
        ),
    }
}

fn parse_id_and_kind(id_or_ext: &str) -> Result<(i32, ResponseKind), Response> {
    let (raw_id, kind) = if let Some(raw_id) = id_or_ext.strip_suffix(".json") {
        (raw_id, ResponseKind::Json)
    } else {
        (id_or_ext, ResponseKind::Text)
    };

    if raw_id.is_empty() {
        return Err(response_error(
            kind,
            StatusCode::BAD_REQUEST,
            "invalid hyperlink id",
        ));
    }

    match raw_id.parse::<i32>() {
        Ok(id) => Ok((id, kind)),
        Err(_) => Err(response_error(
            kind,
            StatusCode::BAD_REQUEST,
            format!("invalid hyperlink id: {raw_id}"),
        )),
    }
}

async fn parse_update_input(
    kind: ResponseKind,
    request: Request,
) -> Result<HyperlinkInput, Response> {
    let body = match body::to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(err) => {
            return Err(response_error(
                kind,
                StatusCode::BAD_REQUEST,
                format!("failed to read request body: {err}"),
            ));
        }
    };

    let parsed = match kind {
        ResponseKind::Text => serde_urlencoded::from_bytes::<HyperlinkInput>(&body)
            .map_err(|err| format!("invalid form payload: {err}")),
        ResponseKind::Json => serde_json::from_slice::<HyperlinkInput>(&body)
            .map_err(|err| format!("invalid json payload: {err}")),
    };

    parsed.map_err(|message| response_error(kind, StatusCode::BAD_REQUEST, message))
}

fn to_response(model: &hyperlink::Model) -> HyperlinkResponse {
    HyperlinkResponse {
        id: model.id,
        title: model.title.clone(),
        url: model.url.clone(),
        clicks_count: model.clicks_count,
        last_clicked_at: model.last_clicked_at.as_ref().map(ToString::to_string),
        processing_state: processing_state_name(model.processing_state.clone()).to_string(),
        created_at: model.created_at.to_string(),
        updated_at: model.updated_at.to_string(),
    }
}

fn processing_state_name(state: HyperlinkProcessingState) -> &'static str {
    match state {
        HyperlinkProcessingState::Waiting => "waiting",
        HyperlinkProcessingState::Processing => "processing",
        HyperlinkProcessingState::Processed => "processed",
        HyperlinkProcessingState::Error => "error",
    }
}

fn show_path(id: i32) -> String {
    format!("/hyperlinks/{id}")
}

fn html_page(title: &str, content: Markup) -> axum::response::Html<String> {
    html_layout::page(title, content)
}

fn render_index(links: &[hyperlink::Model]) -> Markup {
    html! {
        section aria-labelledby="links-heading" {
            @if links.is_empty() {
                p { "No hyperlinks yet." }
            } @else {
                ul class="vstack gap-4" {
                    @for link in links {
                        li class="hyperlink" {
                            @let created_at_iso = link.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
                            @let created_at_human = link
                                .created_at
                                .format("%b %d, %Y %H:%M UTC")
                                .to_string();
                            article class="vstack gap-2" {
                                h3 class="text-lg leading-title" { a href=(&link.url) target="_blank" rel="noopener noreferrer" data-hyperlink-id=(link.id) { (&link.title) } }
                                p class="hstack gap-2 align-center text-sm leading-tight" {
                                    a class="link-cool" href=(&link.url) data-hyperlink-id=(link.id) { (&link.url) }
                                    @if link.url.ends_with(".pdf") {
                                        span class="badge" { "PDF" }
                                    }
                                }
                                div class="hstack gap-2 align-center text-sm leading-tight" {
                                    span {
                                        (maud::PreEscaped(format!(
                                            r#"<relative-time datetime="{created_at_iso}">{created_at_human}</relative-time>"#
                                        )))
                                    }
                                    span class="link-soft" {
                                        (processing_state_name(link.processing_state.clone()))
                                    }
                                    span class="link-soft" { (link.clicks_count) " clicks" }
                                    a class="link-soft" href=(format!("/hyperlinks/{}/edit", link.id)) { "Edit" }
                                    form action=(format!("/hyperlinks/{}/delete", link.id)) method="post" data-confirm="Delete this link?" {
                                        button class="btn-link link-soft" type="submit" { "Delete" }
                                    }
                                }

                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_new() -> Markup {
    html! {
        section aria-labelledby="new-link-heading" {
            h2 { "Add Link" }
            (render_hyperlink_form("/hyperlinks", "Create", "", "", true))
        }
    }
}

fn render_show(link: &hyperlink::Model) -> Markup {
    html! {
        article {
            h2 { (&link.title) }
            p { a href=(&link.url) data-hyperlink-id=(link.id) { (&link.url) } }
            p {
                a href=(format!("/hyperlinks/{}/edit", link.id)) { "Edit" } " "
                a href="/hyperlinks" { "Back to list" }
            }
            form action=(format!("/hyperlinks/{}/delete", link.id)) method="post" data-confirm="Delete this link?" {
                button type="submit" { "Delete" }
            }
        }
    }
}

fn render_edit(link: &hyperlink::Model) -> Markup {
    html! {
        section aria-labelledby="edit-link-heading" {
            h2 id="edit-link-heading" { "Edit Link" }
            (render_hyperlink_form(
                &format!("/hyperlinks/{}/update", link.id),
                "Update",
                &link.title,
                &link.url,
                false
            ))
            p { a href=(show_path(link.id)) { "Back to link" } }
        }
    }
}

fn render_hyperlink_form(
    action: &str,
    submit_label: &str,
    title: &str,
    url: &str,
    is_new_record: bool,
) -> Markup {
    html! {
        form action=(action) method="post" {
            label for="url" { "URL"
                input id="url" name="url" type="url" required value=(url) autofocus;
            }

            @if is_new_record {
                input type="hidden" name="title" value="";
            } @else {
                p {
                    label for="title" { "Title" }
                    input id="title" name="title" type="text" required value=(title);
                }
            }
            button type="submit" { (submit_label) }
        }
    }
}

fn response_error(kind: ResponseKind, status: StatusCode, message: impl Into<String>) -> Response {
    let message = message.into();
    match kind {
        ResponseKind::Text => (
            status,
            html_page(
                "Error",
                html! {
                    section {
                        h2 { (status.as_u16()) " " (status.canonical_reason().unwrap_or("Error")) }
                        p { (message) }
                        p { a href="/hyperlinks" { "Back to hyperlinks" } }
                    }
                },
            ),
        )
            .into_response(),
        ResponseKind::Json => json_error(status, message),
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
    use serde::Serialize;
    use serde_json::json;

    async fn new_server() -> TestServer {
        let connection = Database::connect("sqlite::memory:")
            .await
            .expect("in-memory database should initialize");
        initialize_schema(&connection).await;

        let app = Router::<Context>::new().merge(links()).with_state(Context {
            connection,
            processing_queue: None,
        });
        TestServer::new(app).expect("test server should initialize")
    }

    async fn initialize_schema(connection: &DatabaseConnection) {
        connection
            .execute(Statement::from_string(
                connection.get_database_backend(),
                r#"
                    CREATE TABLE hyperlink (
                        id integer NOT NULL PRIMARY KEY AUTOINCREMENT,
                        title varchar NOT NULL,
                        url varchar NOT NULL,
                        clicks_count integer NOT NULL DEFAULT 0,
                        last_clicked_at datetime_text NULL,
                        processing_state varchar NOT NULL DEFAULT 'waiting',
                        processing_started_at datetime_text NULL,
                        processed_at datetime_text NULL,
                        created_at datetime_text NOT NULL,
                        updated_at datetime_text NOT NULL
                    );
                "#
                .to_string(),
            ))
            .await
            .expect("schema should initialize");
    }

    #[derive(Serialize)]
    struct HtmlForm<'a> {
        title: &'a str,
        url: &'a str,
    }

    fn form_body(title: &str, url: &str) -> String {
        serde_urlencoded::to_string(HtmlForm { title, url }).expect("form should serialize")
    }

    #[tokio::test]
    async fn json_crud_flow_works() {
        let server = new_server().await;

        let create = server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "Example",
                "url": "https://example.com",
            }))
            .await;
        create.assert_status(StatusCode::CREATED);
        let created: HyperlinkResponse = create.json();
        assert_eq!(created.title, "Example");

        let show_path = format!("/hyperlinks/{}.json", created.id);
        let show = server.get(&show_path).await;
        show.assert_status_ok();
        let shown: HyperlinkResponse = show.json();
        assert_eq!(shown.url, "https://example.com");

        let update_path = format!("/hyperlinks/{}.json", created.id);
        let update = server
            .patch(&update_path)
            .json(&json!({
                "title": "Updated",
                "url": "https://updated.example.com",
            }))
            .await;
        update.assert_status_ok();
        let updated: HyperlinkResponse = update.json();
        assert_eq!(updated.title, "Updated");

        let delete_path = format!("/hyperlinks/{}", created.id);
        let delete = server.delete(&delete_path).await;
        delete.assert_status_see_other();

        let after_delete_path = format!("/hyperlinks/{}.json", created.id);
        server
            .get(&after_delete_path)
            .await
            .assert_status_not_found();
    }

    #[tokio::test]
    async fn json_create_autofills_empty_title() {
        let server = new_server().await;

        let created = server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "",
                "url": "https://example.com",
            }))
            .await;
        created.assert_status(StatusCode::CREATED);
        let created_model: HyperlinkResponse = created.json();
        assert_eq!(created_model.title, "https://example.com");

        server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "Example",
                "url": "   ",
            }))
            .await
            .assert_status_bad_request();
    }

    #[tokio::test]
    async fn visit_redirect_increments_click_count() {
        let server = new_server().await;

        let create = server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "Example",
                "url": "https://example.com",
            }))
            .await;
        create.assert_status(StatusCode::CREATED);
        let created: HyperlinkResponse = create.json();
        assert_eq!(created.clicks_count, 0);
        assert!(created.last_clicked_at.is_none());

        let visit = server
            .get(&format!("/hyperlinks/{}/visit", created.id))
            .await;
        visit.assert_status(StatusCode::TEMPORARY_REDIRECT);
        visit.assert_header("location", "https://example.com");

        let show = server
            .get(&format!("/hyperlinks/{}.json", created.id))
            .await;
        show.assert_status_ok();
        let shown: HyperlinkResponse = show.json();
        assert_eq!(shown.clicks_count, 1);
        assert!(shown.last_clicked_at.is_some());
    }

    #[tokio::test]
    async fn click_endpoint_increments_click_count() {
        let server = new_server().await;

        let create = server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "Example",
                "url": "https://example.com",
            }))
            .await;
        create.assert_status(StatusCode::CREATED);
        let created: HyperlinkResponse = create.json();
        assert_eq!(created.clicks_count, 0);
        assert!(created.last_clicked_at.is_none());

        let click = server
            .post(&format!("/hyperlinks/{}/click", created.id))
            .await;
        click.assert_status(StatusCode::NO_CONTENT);

        let show = server
            .get(&format!("/hyperlinks/{}.json", created.id))
            .await;
        show.assert_status_ok();
        let shown: HyperlinkResponse = show.json();
        assert_eq!(shown.clicks_count, 1);
        assert!(shown.last_clicked_at.is_some());
    }

    #[tokio::test]
    async fn html_pages_render() {
        let server = new_server().await;
        let create = server
            .post("/hyperlinks.json")
            .json(&json!({
                "title": "Example",
                "url": "https://example.com",
            }))
            .await;
        create.assert_status(StatusCode::CREATED);
        let created: HyperlinkResponse = create.json();

        let index = server.get("/hyperlinks").await;
        index.assert_status_ok();
        assert!(index.text().contains("<!DOCTYPE html>"));
        assert!(index.text().contains("/hyperlinks/new"));
        assert!(index.text().contains("href=\"https://example.com\""));
        assert!(index.text().contains("data-hyperlink-id=\"1\""));
        assert!(!index.text().contains("/hyperlinks/1/visit"));

        let new_page = server.get("/hyperlinks/new").await;
        new_page.assert_status_ok();
        assert!(
            new_page
                .text()
                .contains("<form action=\"/hyperlinks\" method=\"post\">")
        );

        let show = server.get(&format!("/hyperlinks/{}", created.id)).await;
        show.assert_status_ok();
        assert!(show.text().contains("Edit"));
        assert!(
            show.text()
                .contains(&format!("/hyperlinks/{}/delete", created.id))
        );

        let edit = server
            .get(&format!("/hyperlinks/{}/edit", created.id))
            .await;
        edit.assert_status_ok();
        assert!(
            edit.text()
                .contains(&format!("/hyperlinks/{}/update", created.id))
        );
    }

    #[tokio::test]
    async fn html_write_flows_redirect() {
        let server = new_server().await;

        let create = server
            .post("/hyperlinks")
            .text(form_body("Example", "https://example.com"))
            .content_type("application/x-www-form-urlencoded")
            .await;
        create.assert_status_see_other();
        create.assert_header("location", "/hyperlinks/1");

        let update = server
            .post("/hyperlinks/1/update")
            .text(form_body("Updated", "https://updated.example.com"))
            .content_type("application/x-www-form-urlencoded")
            .await;
        update.assert_status_see_other();
        update.assert_header("location", "/hyperlinks/1");

        let delete = server.post("/hyperlinks/1/delete").await;
        delete.assert_status_see_other();
        delete.assert_header("location", "/hyperlinks");

        server
            .get("/hyperlinks/1.json")
            .await
            .assert_status_not_found();
    }
}
