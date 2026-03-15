use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, header},
    response::Html,
    routing::get,
};
use chrono::{DateTime as ChronoDateTime, SecondsFormat, Utc};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, JoinType, QueryFilter, QueryOrder, QuerySelect,
    RelationTrait,
};
use seaography::{
    Builder, BuilderContext, ConnectionObjectBuilder, EntityObjectBuilder, FilterInputBuilder,
    OrderInputBuilder, PaginationInputBuilder, apply_memory_pagination, apply_order,
    apply_pagination, async_graphql,
    async_graphql::{
        Name, Request, Response, ServerError,
        dynamic::{
            Enum, Field, FieldFuture, FieldValue, InputValue, Object, Schema, SchemaError, TypeRef,
        },
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

const UPDATED_HYPERLINKS_PAYLOAD_TYPE: &str = "UpdatedHyperlinksPayload";
const UPDATED_HYPERLINK_CHANGE_TYPE: &str = "UpdatedHyperlinkChange";
const HYPERLINK_CHANGE_TYPE_ENUM: &str = "HyperlinkChangeType";
const HYPERLINK_REF_TYPE: &str = "HyperlinkRef";
const FIELD_UPDATED_AT: &str = "updatedAt";

#[derive(Clone, Debug)]
struct UpdatedHyperlinksPayload {
    server_updated_at: sea_orm::entity::prelude::DateTime,
    changes: Vec<UpdatedHyperlinkChange>,
}

#[derive(Clone, Debug)]
struct UpdatedHyperlinkChange {
    id: i32,
    change_type: HyperlinkChangeType,
    updated_at: sea_orm::entity::prelude::DateTime,
    hyperlink: Option<hyperlink::Model>,
}

#[derive(Clone, Debug)]
struct HyperlinkRef {
    id: i32,
    title: String,
    url: String,
    raw_url: String,
}

impl From<hyperlink::Model> for HyperlinkRef {
    fn from(model: hyperlink::Model) -> Self {
        Self {
            id: model.id,
            title: model.title,
            url: model.url,
            raw_url: model.raw_url,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum HyperlinkChangeType {
    Updated,
    Deleted,
}

impl HyperlinkChangeType {
    fn graphql_name(self) -> &'static str {
        match self {
            Self::Updated => "UPDATED",
            Self::Deleted => "DELETED",
        }
    }

    fn sort_rank(self) -> i32 {
        match self {
            Self::Updated => 0,
            Self::Deleted => 1,
        }
    }
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

pub fn export_schema_sdl(connection: DatabaseConnection) -> Result<String, SchemaError> {
    schema(
        connection,
        GraphqlRequestBaseUrl("http://localhost:8765".to_string()),
    )
    .map(|built| built.sdl())
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
    register_updated_hyperlinks_query_field(&mut builder);
    register_read_only_entity!(builder, hyperlink_processing_job);
    register_read_only_entity!(builder, hyperlink_artifact);
    register_hyperlink_sublinks_field(&mut builder);
    builder.outputs.push(hyperlink_ref_object());
    register_hyperlink_discovered_via_field(&mut builder);
    register_hyperlink_artifact_url_fields(&mut builder);

    builder.register_enumeration::<hyperlink_processing_job::HyperlinkProcessingJobState>();
    builder.register_enumeration::<hyperlink_processing_job::HyperlinkProcessingJobKind>();
    builder.register_enumeration::<hyperlink::HyperlinkSourceType>();
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

fn register_updated_hyperlinks_query_field(builder: &mut Builder) {
    let context = builder.context;
    let entity_object_builder = EntityObjectBuilder { context };
    let hyperlink_type_name = entity_object_builder.type_name::<hyperlink::Entity>();

    builder.enumerations.push(
        Enum::new(HYPERLINK_CHANGE_TYPE_ENUM)
            .item("UPDATED")
            .item("DELETED"),
    );
    builder.outputs.push(updated_hyperlinks_payload_object());
    builder
        .outputs
        .push(updated_hyperlink_change_object(hyperlink_type_name));

    let updated_hyperlinks_field = Field::new(
        "updatedHyperlinks",
        TypeRef::named_nn(UPDATED_HYPERLINKS_PAYLOAD_TYPE),
        move |ctx| {
            FieldFuture::new(async move {
                let db = ctx.data::<DatabaseConnection>()?;
                let updated_at_raw = ctx.args.try_get(FIELD_UPDATED_AT)?.string()?;
                let updated_after =
                    parse_updated_at_cursor(updated_at_raw).map_err(async_graphql::Error::new)?;
                let payload = load_updated_hyperlinks_payload(db, updated_after).await?;
                Ok(Some(FieldValue::owned_any(payload)))
            })
        },
    )
    .argument(InputValue::new(
        FIELD_UPDATED_AT,
        TypeRef::named_nn(TypeRef::STRING),
    ));

    builder.queries.push(updated_hyperlinks_field);
}

fn updated_hyperlinks_payload_object() -> Object {
    Object::new(UPDATED_HYPERLINKS_PAYLOAD_TYPE)
        .field(Field::new(
            "serverUpdatedAt",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let payload = ctx
                        .parent_value
                        .try_downcast_ref::<UpdatedHyperlinksPayload>()?;
                    Ok(Some(FieldValue::value(format_graphql_datetime(
                        &payload.server_updated_at,
                    ))))
                })
            },
        ))
        .field(Field::new(
            "changes",
            TypeRef::named_nn_list_nn(UPDATED_HYPERLINK_CHANGE_TYPE),
            |ctx| {
                FieldFuture::new(async move {
                    let payload = ctx
                        .parent_value
                        .try_downcast_ref::<UpdatedHyperlinksPayload>()?;
                    let values = payload
                        .changes
                        .iter()
                        .cloned()
                        .map(FieldValue::owned_any)
                        .collect::<Vec<_>>();
                    Ok(Some(FieldValue::list(values)))
                })
            },
        ))
}

fn updated_hyperlink_change_object(hyperlink_type_name: String) -> Object {
    Object::new(UPDATED_HYPERLINK_CHANGE_TYPE)
        .field(Field::new("id", TypeRef::named_nn(TypeRef::INT), |ctx| {
            FieldFuture::new(async move {
                let change = ctx
                    .parent_value
                    .try_downcast_ref::<UpdatedHyperlinkChange>()?;
                Ok(Some(FieldValue::value(change.id)))
            })
        }))
        .field(Field::new(
            "changeType",
            TypeRef::named_nn(HYPERLINK_CHANGE_TYPE_ENUM),
            |ctx| {
                FieldFuture::new(async move {
                    let change = ctx
                        .parent_value
                        .try_downcast_ref::<UpdatedHyperlinkChange>()?;
                    Ok(Some(FieldValue::value(Name::new(
                        change.change_type.graphql_name(),
                    ))))
                })
            },
        ))
        .field(Field::new(
            "updatedAt",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let change = ctx
                        .parent_value
                        .try_downcast_ref::<UpdatedHyperlinkChange>()?;
                    Ok(Some(FieldValue::value(format_graphql_datetime(
                        &change.updated_at,
                    ))))
                })
            },
        ))
        .field(Field::new(
            "hyperlink",
            TypeRef::named(hyperlink_type_name),
            |ctx| {
                FieldFuture::new(async move {
                    let change = ctx
                        .parent_value
                        .try_downcast_ref::<UpdatedHyperlinkChange>()?;
                    Ok(change
                        .hyperlink
                        .as_ref()
                        .cloned()
                        .map(FieldValue::owned_any))
                })
            },
        ))
}

async fn load_updated_hyperlinks_payload(
    connection: &DatabaseConnection,
    updated_after: sea_orm::entity::prelude::DateTime,
) -> Result<UpdatedHyperlinksPayload, sea_orm::DbErr> {
    let updated_hyperlinks =
        crate::app::models::hyperlink::list_updated_after(connection, updated_after).await?;
    let deleted_hyperlinks =
        crate::app::models::hyperlink_tombstone::list_updated_after(connection, updated_after)
            .await?;

    let mut changes = Vec::with_capacity(updated_hyperlinks.len() + deleted_hyperlinks.len());

    changes.extend(
        updated_hyperlinks
            .into_iter()
            .map(|model| UpdatedHyperlinkChange {
                id: model.id,
                change_type: HyperlinkChangeType::Updated,
                updated_at: model.updated_at,
                hyperlink: Some(model),
            }),
    );

    changes.extend(
        deleted_hyperlinks
            .into_iter()
            .map(|model| UpdatedHyperlinkChange {
                id: model.hyperlink_id,
                change_type: HyperlinkChangeType::Deleted,
                updated_at: model.updated_at,
                hyperlink: None,
            }),
    );

    changes.sort_by(|left, right| {
        left.updated_at
            .cmp(&right.updated_at)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| {
                left.change_type
                    .sort_rank()
                    .cmp(&right.change_type.sort_rank())
            })
    });

    let server_updated_at = changes
        .last()
        .map(|change| change.updated_at)
        .unwrap_or_else(now_utc);

    Ok(UpdatedHyperlinksPayload {
        server_updated_at,
        changes,
    })
}

fn parse_updated_at_cursor(value: &str) -> Result<sea_orm::entity::prelude::DateTime, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("updatedAt must be RFC3339 timestamp".to_string());
    }

    ChronoDateTime::parse_from_rfc3339(trimmed)
        .map(|parsed| parsed.with_timezone(&Utc).naive_utc())
        .map_err(|_| "updatedAt must be RFC3339 timestamp".to_string())
}

fn format_graphql_datetime(value: &sea_orm::entity::prelude::DateTime) -> String {
    ChronoDateTime::<Utc>::from_naive_utc_and_offset(*value, Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn now_utc() -> sea_orm::entity::prelude::DateTime {
    sea_orm::entity::prelude::DateTimeUtc::from(std::time::SystemTime::now()).naive_utc()
}

fn hyperlink_ref_object() -> Object {
    Object::new(HYPERLINK_REF_TYPE)
        .field(Field::new("id", TypeRef::named_nn(TypeRef::INT), |ctx| {
            FieldFuture::new(async move {
                let hyperlink_ref = ctx.parent_value.try_downcast_ref::<HyperlinkRef>()?;
                Ok(Some(FieldValue::value(hyperlink_ref.id)))
            })
        }))
        .field(Field::new(
            "title",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let hyperlink_ref = ctx.parent_value.try_downcast_ref::<HyperlinkRef>()?;
                    Ok(Some(FieldValue::value(hyperlink_ref.title.clone())))
                })
            },
        ))
        .field(Field::new(
            "url",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let hyperlink_ref = ctx.parent_value.try_downcast_ref::<HyperlinkRef>()?;
                    Ok(Some(FieldValue::value(hyperlink_ref.url.clone())))
                })
            },
        ))
        .field(Field::new(
            "rawUrl",
            TypeRef::named_nn(TypeRef::STRING),
            |ctx| {
                FieldFuture::new(async move {
                    let hyperlink_ref = ctx.parent_value.try_downcast_ref::<HyperlinkRef>()?;
                    Ok(Some(FieldValue::value(hyperlink_ref.raw_url.clone())))
                })
            },
        ))
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

fn register_hyperlink_discovered_via_field(builder: &mut Builder) {
    let context = builder.context;
    let entity_object_builder = EntityObjectBuilder { context };
    let hyperlink_type_name = entity_object_builder.type_name::<hyperlink::Entity>();

    let discovered_via_field = Field::new(
        "discoveredVia",
        TypeRef::named_nn_list_nn(HYPERLINK_REF_TYPE),
        |ctx| {
            FieldFuture::new(async move {
                let hyperlink = ctx
                    .parent_value
                    .try_downcast_ref::<hyperlink::Model>()
                    .expect("parent hyperlink should exist");
                let db = ctx.data::<DatabaseConnection>()?;

                let parents = hyperlink::Entity::find()
                    .join(
                        JoinType::InnerJoin,
                        hyperlink_relation::Relation::ParentHyperlink.def().rev(),
                    )
                    .filter(hyperlink_relation::Column::ChildHyperlinkId.eq(hyperlink.id))
                    .order_by_desc(hyperlink_relation::Column::CreatedAt)
                    .order_by_desc(hyperlink_relation::Column::Id)
                    .all(db)
                    .await?;

                let mut seen_parent_ids = HashSet::with_capacity(parents.len());
                let discovered_via = parents
                    .into_iter()
                    .filter_map(|parent| {
                        if seen_parent_ids.insert(parent.id) {
                            Some(FieldValue::owned_any(HyperlinkRef::from(parent)))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();

                Ok(Some(FieldValue::list(discovered_via)))
            })
        },
    );

    let mut discovered_via_field = Some(discovered_via_field);
    builder.outputs = builder
        .outputs
        .drain(..)
        .map(|object| {
            if object.type_name() == hyperlink_type_name {
                object.field(
                    discovered_via_field
                        .take()
                        .expect("discoveredVia field should only be added once"),
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
        "screenshot_webp",
    ));
    let mut screenshot_dark_url_field = Some(hyperlink_artifact_url_field(
        "screenshotDarkUrl",
        "screenshot_dark_webp",
    ));
    let mut thumbnail_url_field = Some(hyperlink_artifact_url_field(
        "thumbnailUrl",
        "screenshot_thumb_webp",
    ));
    let mut thumbnail_dark_url_field = Some(hyperlink_artifact_url_field(
        "thumbnailDarkUrl",
        "screenshot_thumb_dark_webp",
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
#[path = "../../tests/unit/server_graphql.rs"]
mod tests;
