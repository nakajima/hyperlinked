use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, header},
    response::Html,
    routing::get,
};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, JoinType, QueryFilter, QuerySelect, RelationTrait,
};
use seaography::{
    Builder, BuilderContext, ConnectionObjectBuilder, EntityObjectBuilder, FilterInputBuilder,
    OrderInputBuilder, PaginationInputBuilder, apply_memory_pagination, apply_order,
    apply_pagination, async_graphql,
    async_graphql::{
        Request, Response, ServerError,
        dynamic::{Field, FieldFuture, FieldValue, InputValue, Schema, SchemaError, TypeRef},
        http::graphiql_source,
        parser::types::{DocumentOperations, OperationType},
    },
    heck::ToLowerCamelCase,
    lazy_static,
};

use crate::{
    entity::{hyperlink, hyperlink_artifact, hyperlink_processing_job, hyperlink_relation},
    server::{
        context::Context,
        hyperlink_fetcher::{HyperlinkFetchQuery, HyperlinkFetcher},
    },
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

#[derive(Clone, Debug)]
struct GraphqlRequestBaseUrl(String);

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

async fn execute(
    State(state): State<Context>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Json<Response> {
    if operation_type(&request) == Some(OperationType::Mutation) {
        return Json(read_only_response());
    }

    let request_base_url = GraphqlRequestBaseUrl(
        derive_request_base_url(&headers).unwrap_or_else(|| "http://localhost:8765".to_string()),
    );

    let schema = match schema(state.connection.clone(), request_base_url) {
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

fn schema(
    connection: DatabaseConnection,
    request_base_url: GraphqlRequestBaseUrl,
) -> Result<Schema, SchemaError> {
    let mut builder = Builder::new(&CONTEXT, connection.clone());

    let hyperlinks_query_index = builder.queries.len();
    register_read_only_entity!(builder, hyperlink);
    register_hyperlinks_query_field(&mut builder, hyperlinks_query_index);
    register_read_only_entity!(builder, hyperlink_processing_job);
    register_read_only_entity!(builder, hyperlink_artifact);
    register_hyperlink_sublinks_field(&mut builder);
    register_hyperlink_artifact_url_fields(&mut builder);

    builder.register_enumeration::<hyperlink_processing_job::HyperlinkProcessingJobState>();
    builder.register_enumeration::<hyperlink_processing_job::HyperlinkProcessingJobKind>();
    builder.register_enumeration::<hyperlink_artifact::HyperlinkArtifactKind>();

    builder
        .schema_builder()
        .data(connection)
        .data(request_base_url)
        .finish()
}

fn register_hyperlinks_query_field(builder: &mut Builder, query_index: usize) {
    let context = builder.context;
    let entity_object_builder = EntityObjectBuilder { context };
    let connection_object_builder = ConnectionObjectBuilder { context };
    let filter_input_builder = FilterInputBuilder { context };
    let order_input_builder = OrderInputBuilder { context };
    let pagination_input_builder = PaginationInputBuilder { context };

    let hyperlink_type_name = entity_object_builder.type_name::<hyperlink::Entity>();
    let hyperlink_connection_type_name = connection_object_builder.type_name(&hyperlink_type_name);
    let field_filters_name = context.entity_query_field.filters.clone();
    let field_order_by_name = context.entity_query_field.order_by.clone();
    let field_pagination_name = context.entity_query_field.pagination.clone();
    let resolver_filters_name = field_filters_name.clone();
    let resolver_order_by_name = field_order_by_name.clone();
    let resolver_pagination_name = field_pagination_name.clone();
    const FIELD_Q: &str = "q";

    let hyperlinks_field = Field::new(
        "hyperlinks",
        TypeRef::named_nn(hyperlink_connection_type_name),
        move |ctx| {
            let context = context;
            let field_filters_name = resolver_filters_name.clone();
            let field_order_by_name = resolver_order_by_name.clone();
            let field_pagination_name = resolver_pagination_name.clone();
            FieldFuture::new(async move {
                let db = ctx.data::<DatabaseConnection>()?;
                let pagination = ctx.args.get(&field_pagination_name);
                let pagination = PaginationInputBuilder { context }.parse_object(pagination);
                let q = match ctx.args.get(FIELD_Q) {
                    Some(value) if value.is_null() => None,
                    Some(value) => {
                        let parsed = value.string()?.trim().to_owned();
                        if parsed.is_empty() {
                            None
                        } else {
                            Some(parsed)
                        }
                    }
                    None => None,
                };

                if let Some(q) = q {
                    let results = HyperlinkFetcher::new(
                        db,
                        HyperlinkFetchQuery {
                            q: Some(q),
                            ..Default::default()
                        },
                    )
                    .fetch()
                    .await?;
                    let connection = apply_memory_pagination::<hyperlink::Entity>(
                        Some(results.links),
                        pagination,
                    );
                    return Ok(Some(FieldValue::owned_any(connection)));
                }

                let filters = ctx.args.get(&field_filters_name);
                let filters =
                    seaography::get_filter_conditions::<hyperlink::Entity>(context, filters);
                let order_by = ctx.args.get(&field_order_by_name);
                let order_by =
                    OrderInputBuilder { context }.parse_object::<hyperlink::Entity>(order_by);
                let stmt = hyperlink::Entity::find().filter(filters);
                let stmt = apply_order(stmt, order_by);
                let connection =
                    apply_pagination::<hyperlink::Entity>(db, stmt, pagination).await?;
                Ok(Some(FieldValue::owned_any(connection)))
            })
        },
    )
    .argument(InputValue::new(
        &field_filters_name,
        TypeRef::named(filter_input_builder.type_name(&hyperlink_type_name)),
    ))
    .argument(InputValue::new(
        &field_order_by_name,
        TypeRef::named(order_input_builder.type_name(&hyperlink_type_name)),
    ))
    .argument(InputValue::new(
        &field_pagination_name,
        TypeRef::named(pagination_input_builder.type_name()),
    ))
    .argument(InputValue::new(FIELD_Q, TypeRef::named(TypeRef::STRING)));

    builder.queries[query_index] = hyperlinks_field;
}

fn register_hyperlink_sublinks_field(builder: &mut Builder) {
    let context = builder.context;
    let entity_object_builder = EntityObjectBuilder { context };
    let connection_object_builder = ConnectionObjectBuilder { context };
    let filter_input_builder = FilterInputBuilder { context };
    let order_input_builder = OrderInputBuilder { context };

    let hyperlink_type_name = entity_object_builder.type_name::<hyperlink::Entity>();
    let hyperlink_connection_type_name = connection_object_builder.type_name(&hyperlink_type_name);
    let field_filters_name = context.entity_query_field.filters.clone();
    let field_order_by_name = context.entity_query_field.order_by.clone();
    let field_pagination_name = context.entity_query_field.pagination.clone();
    let resolver_filters_name = field_filters_name.clone();
    let resolver_order_by_name = field_order_by_name.clone();
    let resolver_pagination_name = field_pagination_name.clone();
    let pagination_input_type_name = context.pagination_input.type_name.clone();

    let sublinks_field = Field::new(
        "sublinks",
        TypeRef::named_nn(hyperlink_connection_type_name),
        move |ctx| {
            let context = context;
            let field_filters_name = resolver_filters_name.clone();
            let field_order_by_name = resolver_order_by_name.clone();
            let field_pagination_name = resolver_pagination_name.clone();
            FieldFuture::new(async move {
                let parent = ctx
                    .parent_value
                    .try_downcast_ref::<hyperlink::Model>()
                    .expect("parent hyperlink should exist");
                let db = ctx.data::<DatabaseConnection>()?;

                let filters = ctx.args.get(&field_filters_name);
                let filters =
                    seaography::get_filter_conditions::<hyperlink::Entity>(context, filters);
                let order_by = ctx.args.get(&field_order_by_name);
                let order_by =
                    OrderInputBuilder { context }.parse_object::<hyperlink::Entity>(order_by);
                let pagination = ctx.args.get(&field_pagination_name);
                let pagination = PaginationInputBuilder { context }.parse_object(pagination);

                let stmt = hyperlink::Entity::find()
                    .join(
                        JoinType::InnerJoin,
                        hyperlink_relation::Relation::ChildHyperlink.def().rev(),
                    )
                    .filter(hyperlink_relation::Column::ParentHyperlinkId.eq(parent.id))
                    .filter(filters);
                let stmt = apply_order(stmt, order_by);
                let connection =
                    apply_pagination::<hyperlink::Entity>(db, stmt, pagination).await?;

                Ok(Some(FieldValue::owned_any(connection)))
            })
        },
    )
    .argument(InputValue::new(
        &field_filters_name,
        TypeRef::named(filter_input_builder.type_name(&hyperlink_type_name)),
    ))
    .argument(InputValue::new(
        &field_order_by_name,
        TypeRef::named(order_input_builder.type_name(&hyperlink_type_name)),
    ))
    .argument(InputValue::new(
        &field_pagination_name,
        TypeRef::named(&pagination_input_type_name),
    ));

    let mut sublinks_field = Some(sublinks_field);
    builder.outputs = builder
        .outputs
        .drain(..)
        .map(|object| {
            if object.type_name() == hyperlink_type_name {
                object.field(
                    sublinks_field
                        .take()
                        .expect("sublinks field should only be added once"),
                )
            } else {
                object
            }
        })
        .collect();
}

fn register_hyperlink_artifact_url_fields(builder: &mut Builder) {
    let context = builder.context;
    let entity_object_builder = EntityObjectBuilder { context };
    let hyperlink_type_name = entity_object_builder.type_name::<hyperlink::Entity>();

    let mut screenshot_url_field = Some(hyperlink_artifact_url_field(
        "screenshotUrl",
        "screenshot_png",
    ));
    let mut screenshot_dark_url_field = Some(hyperlink_artifact_url_field(
        "screenshotDarkUrl",
        "screenshot_dark_png",
    ));
    let mut thumbnail_url_field = Some(hyperlink_artifact_url_field(
        "thumbnailUrl",
        "screenshot_thumb_png",
    ));
    let mut thumbnail_dark_url_field = Some(hyperlink_artifact_url_field(
        "thumbnailDarkUrl",
        "screenshot_thumb_dark_png",
    ));

    builder.outputs = builder
        .outputs
        .drain(..)
        .map(|object| {
            if object.type_name() == hyperlink_type_name {
                object
                    .field(
                        screenshot_url_field
                            .take()
                            .expect("screenshotUrl field should only be added once"),
                    )
                    .field(
                        screenshot_dark_url_field
                            .take()
                            .expect("screenshotDarkUrl field should only be added once"),
                    )
                    .field(
                        thumbnail_url_field
                            .take()
                            .expect("thumbnailUrl field should only be added once"),
                    )
                    .field(
                        thumbnail_dark_url_field
                            .take()
                            .expect("thumbnailDarkUrl field should only be added once"),
                    )
            } else {
                object
            }
        })
        .collect();
}

fn hyperlink_artifact_url_field(field_name: &'static str, artifact_slug: &'static str) -> Field {
    Field::new(field_name, TypeRef::named(TypeRef::STRING), move |ctx| {
        FieldFuture::new(async move {
            let parent = ctx
                .parent_value
                .try_downcast_ref::<hyperlink::Model>()
                .expect("parent hyperlink should exist");
            let Some(request_base_url) = ctx.data_opt::<GraphqlRequestBaseUrl>() else {
                return Ok(None);
            };
            let base = request_base_url.0.trim_end_matches('/');
            if base.is_empty() {
                return Ok(None);
            }

            let url = format!(
                "{base}/hyperlinks/{}/artifacts/{artifact_slug}/inline",
                parent.id
            );
            Ok(Some(FieldValue::value(url)))
        })
    })
}

fn derive_request_base_url(headers: &HeaderMap) -> Option<String> {
    let host = first_header_value(headers, "x-forwarded-host")
        .or_else(|| first_header_value(headers, header::HOST.as_str()))?;
    let proto = first_header_value(headers, "x-forwarded-proto")
        .or_else(|| first_header_value(headers, "x-forwarded-scheme"))
        .unwrap_or_else(|| "http".to_string());
    let proto = proto.trim_end_matches(':');
    if proto.is_empty() {
        return None;
    }

    Some(format!("{proto}://{}", host.trim_end_matches('/')))
}

fn first_header_value(headers: &HeaderMap, key: &str) -> Option<String> {
    let raw = headers.get(key)?.to_str().ok()?;
    let first = raw.split(',').next()?.trim();
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
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
                INSERT INTO hyperlink_relation (id, parent_hyperlink_id, child_hyperlink_id, created_at)
                VALUES (1, 1, 2, '2026-02-19 00:00:11');
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
                  thumbnailUrl
                  thumbnailDarkUrl
                  screenshotUrl
                  screenshotDarkUrl
                  sublinks(
                    pagination: { page: { limit: 10, page: 0 } }
                    orderBy: { id: ASC }
                  ) {
                    nodes { id url }
                  }
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
        assert!(
            payload["data"]["hyperlinks"]["nodes"][0]["thumbnailUrl"]
                .as_str()
                .unwrap_or("")
                .ends_with("/hyperlinks/1/artifacts/screenshot_thumb_png/inline")
        );
        assert!(
            payload["data"]["hyperlinks"]["nodes"][0]["thumbnailDarkUrl"]
                .as_str()
                .unwrap_or("")
                .ends_with("/hyperlinks/1/artifacts/screenshot_thumb_dark_png/inline")
        );
        assert!(
            payload["data"]["hyperlinks"]["nodes"][0]["screenshotUrl"]
                .as_str()
                .unwrap_or("")
                .ends_with("/hyperlinks/1/artifacts/screenshot_png/inline")
        );
        assert!(
            payload["data"]["hyperlinks"]["nodes"][0]["screenshotDarkUrl"]
                .as_str()
                .unwrap_or("")
                .ends_with("/hyperlinks/1/artifacts/screenshot_dark_png/inline")
        );
        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"][0]["sublinks"]["nodes"][0]["id"],
            2
        );
        assert_eq!(
            payload["data"]["hyperlinks"]["nodes"][0]["sublinks"]["nodes"][0]["url"],
            "https://example.com/child"
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
    async fn graphql_hyperlinks_supports_q_argument() {
        let server = new_server().await;
        let payload = run_graphql(
            &server,
            r#"
            {
              hyperlinks(
                q: "with:discovered status:idle"
                pagination: { page: { limit: 10, page: 0 } }
              ) {
                nodes { id title discoveryDepth }
              }
            }
            "#,
        )
        .await;

        let nodes = payload["data"]["hyperlinks"]["nodes"]
            .as_array()
            .expect("nodes should be an array");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0]["id"], 2);
        assert_eq!(nodes[0]["title"], "Discovered Child");
        assert_eq!(nodes[0]["discoveryDepth"], 1);
    }

    #[tokio::test]
    async fn graphql_hyperlinks_allows_null_q_argument() {
        let server = new_server().await;
        let payload = run_graphql(
            &server,
            r#"
            {
              hyperlinks(
                q: null
                pagination: { page: { limit: 10, page: 0 } }
                orderBy: { id: ASC }
              ) {
                nodes { id }
              }
            }
            "#,
        )
        .await;

        let nodes = payload["data"]["hyperlinks"]["nodes"]
            .as_array()
            .expect("nodes should be an array");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["id"], 1);
        assert_eq!(nodes[1]["id"], 2);
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
