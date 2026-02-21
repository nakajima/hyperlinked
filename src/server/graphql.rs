use axum::{Json, Router, extract::State, response::Html, routing::get};
use sea_orm::DatabaseConnection;
use seaography::{
    Builder, BuilderContext, async_graphql,
    async_graphql::{
        Request, Response, ServerError,
        dynamic::{Schema, SchemaError},
        http::graphiql_source,
        parser::types::{DocumentOperations, OperationType},
    },
    heck::ToLowerCamelCase,
    lazy_static,
};

use crate::{
    entity::{hyperlink, hyperlink_artifact, hyperlink_processing_job},
    server::context::Context,
};

lazy_static::lazy_static! {
    static ref CONTEXT: BuilderContext = {
        let mut context = BuilderContext::default();
        context.entity_query_field.type_name = Box::new(|object_name: &str| -> String {
            if object_name == "Hyperlink" {
                "hyperlinks".to_string()
            } else {
                object_name.to_lower_camel_case()
            }
        });
        context
    };
}

macro_rules! register_read_only_entity {
    ($builder:ident, $module_path:ident) => {
        $builder.register_entity::<$module_path::Entity>(
            <$module_path::RelatedEntity as sea_orm::Iterable>::iter()
                .map(|rel| seaography::RelationBuilder::get_relation(&rel, $builder.context))
                .collect(),
        );
        $builder =
            $builder.register_entity_dataloader_one_to_one($module_path::Entity, tokio::spawn);
        $builder =
            $builder.register_entity_dataloader_one_to_many($module_path::Entity, tokio::spawn);
    };
}

pub fn routes() -> Router<Context> {
    Router::new().route("/graphql", get(playground).post(execute))
}

async fn playground() -> Html<String> {
    Html(graphiql_source("/graphql", None))
}

async fn execute(State(state): State<Context>, Json(request): Json<Request>) -> Json<Response> {
    if operation_type(&request) == Some(OperationType::Mutation) {
        return Json(read_only_response());
    }

    let schema = match schema(state.connection.clone()) {
        Ok(schema) => schema,
        Err(error) => {
            return Json(Response::from_errors(vec![ServerError::new(
                format!("failed to build graphql schema: {error}"),
                None,
            )]));
        }
    };

    Json(schema.execute(request).await)
}

fn schema(connection: DatabaseConnection) -> Result<Schema, SchemaError> {
    let mut builder = Builder::new(&CONTEXT, connection.clone());

    register_read_only_entity!(builder, hyperlink);
    register_read_only_entity!(builder, hyperlink_processing_job);
    register_read_only_entity!(builder, hyperlink_artifact);

    builder.register_enumeration::<hyperlink_processing_job::HyperlinkProcessingJobState>();
    builder.register_enumeration::<hyperlink_processing_job::HyperlinkProcessingJobKind>();
    builder.register_enumeration::<hyperlink_artifact::HyperlinkArtifactKind>();

    builder.schema_builder().data(connection).finish()
}

fn operation_type(request: &Request) -> Option<OperationType> {
    let document = async_graphql::parser::parse_query(&request.query).ok()?;

    match (&request.operation_name, &document.operations) {
        (Some(_), DocumentOperations::Single(_)) => None,
        (Some(operation_name), DocumentOperations::Multiple(operations)) => operations
            .get(operation_name.as_str())
            .map(|operation| operation.node.ty),
        (None, DocumentOperations::Single(operation)) => Some(operation.node.ty),
        (None, DocumentOperations::Multiple(operations)) if operations.len() == 1 => operations
            .values()
            .next()
            .map(|operation| operation.node.ty),
        (None, DocumentOperations::Multiple(_)) => None,
    }
}

fn read_only_response() -> Response {
    Response::from_errors(vec![ServerError::new(
        "mutations are disabled on this endpoint",
        None,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::test_support;
    use axum_test::TestServer;
    use serde_json::{Value, json};

    async fn new_server() -> TestServer {
        let connection = test_support::new_memory_connection().await;
        test_support::initialize_hyperlinks_schema(&connection).await;
        test_support::execute_sql(
            &connection,
            r#"
                INSERT INTO hyperlink (id, title, url, raw_url, discovery_depth, clicks_count, last_clicked_at, created_at, updated_at)
                VALUES
                    (1, 'Example', 'https://example.com', 'https://example.com?utm_source=newsletter', 0, 2, NULL, '2026-02-19 00:00:00', '2026-02-19 00:00:00'),
                    (2, 'Discovered Child', 'https://example.com/child', 'https://example.com/child', 1, 0, NULL, '2026-02-19 00:00:10', '2026-02-19 00:00:10');
                INSERT INTO hyperlink_processing_job (id, hyperlink_id, kind, state, error_message, queued_at, started_at, finished_at, created_at, updated_at)
                VALUES (1, 1, 'snapshot', 'queued', NULL, '2026-02-19 00:00:01', NULL, NULL, '2026-02-19 00:00:01', '2026-02-19 00:00:01');
                INSERT INTO hyperlink_artifact (id, hyperlink_id, job_id, kind, payload, content_type, size_bytes, created_at)
                VALUES (1, 1, 1, 'snapshot_warc', x'01AB', 'application/warc', 2, '2026-02-19 00:00:02'),
                       (2, 1, 1, 'pdf_source', x'25504446', 'application/pdf', 4, '2026-02-19 00:00:03');
            "#,
        )
        .await;

        let app = Router::<Context>::new()
            .merge(routes())
            .with_state(Context {
                connection,
                processing_queue: None,
            });

        TestServer::new(app).expect("test server should initialize")
    }

    async fn run_graphql(server: &TestServer, query: &str) -> Value {
        let response = server
            .post("/graphql")
            .json(&json!({ "query": query }))
            .await;
        response.assert_status_ok();
        response.json()
    }

    #[tokio::test]
    async fn graphql_query_uses_seaography_connection_shape() {
        let server = new_server().await;
        let payload = run_graphql(
            &server,
            r#"
            {
              hyperlinks(
                filters: { discoveryDepth: { eq: 0 } }
                pagination: { page: { limit: 10, page: 0 } }
                orderBy: { id: ASC }
              ) {
                nodes {
                  id
                  title
                  url
                  rawUrl
                  discoveryDepth
                  hyperlinkProcessingJob(
                    pagination: { page: { limit: 10, page: 0 } }
                    orderBy: { id: ASC }
                  ) {
                    nodes { kind state }
                  }
                }
              }
            }
            "#,
        )
        .await;

        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"][0]["title"],
            "Example"
        );
        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"][0]["rawUrl"],
            "https://example.com?utm_source=newsletter"
        );
        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"][0]["discoveryDepth"],
            0
        );
        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"]
                .as_array()
                .expect("nodes should be an array")
                .len(),
            1
        );
        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"][0]["hyperlinkProcessingJob"]["nodes"][0]["state"],
            "queued"
        );
    }

    #[tokio::test]
    async fn graphql_query_artifacts_connection_works() {
        let server = new_server().await;
        let payload = run_graphql(
            &server,
            r#"
            {
              hyperlinkArtifact(
                pagination: { page: { limit: 10, page: 0 } }
                orderBy: { id: ASC }
              ) {
                nodes { kind contentType }
              }
            }
            "#,
        )
        .await;

        assert_eq!(
            payload["data"]["hyperlinkArtifact"]["nodes"]
                .as_array()
                .expect("nodes should be an array")
                .len(),
            2
        );
        assert_eq!(
            payload["data"]["hyperlinkArtifact"]["nodes"][1]["contentType"],
            "application/pdf"
        );
    }

    #[tokio::test]
    async fn graphql_mutation_is_rejected() {
        let server = new_server().await;
        let payload = run_graphql(&server, "mutation { _ping }").await;
        let errors = payload["errors"].as_array().expect("errors should exist");
        assert!(
            !errors.is_empty(),
            "mutation should fail on read-only schema"
        );
        let first_error_message = errors
            .first()
            .and_then(|item| item.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        assert!(
            first_error_message.contains("mutations are disabled"),
            "expected read-only error, got: {first_error_message}"
        );
    }

    #[tokio::test]
    async fn graphiql_is_available() {
        let server = new_server().await;
        let page = server.get("/graphql").await;
        page.assert_status_ok();
        assert!(page.text().contains("GraphiQL"));
    }
}
