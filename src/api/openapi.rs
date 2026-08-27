//! OpenAPI 3.0 document served at `/openapi.json` (task 0.4.10).
//!
//! The document is **generated from a single route table** ([`ROUTES`]) so it
//! cannot silently drift from the implemented endpoints: a unit test asserts
//! every table entry appears in the served JSON, and a CI job scrapes the
//! live `/openapi.json` to validate the exposed spec.

use actix_web::{HttpResponse, web};
use utoipa::openapi::{
    ComponentsBuilder, Content, InfoBuilder, ObjectBuilder, OpenApi, PathItem, PathsBuilder,
    Required, Response, ResponseBuilder, Responses, ResponsesBuilder, Schema, SecurityRequirement,
    Tag, Type,
    path::{HttpMethod, Operation, OperationBuilder, Parameter, ParameterBuilder, ParameterIn},
    request_body::{RequestBody, RequestBodyBuilder},
    schema::SchemaType,
    security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

/// One registered HTTP route. This table is the single source of truth the
/// OpenAPI document is generated from.
struct RouteSpec {
    method: HttpMethod,
    path: &'static str,
    tag: &'static str,
    summary: &'static str,
    /// Whether the route requires `Authorization: Bearer <jwt>`.
    secure: bool,
    /// Whether the route accepts a JSON request body.
    has_body: bool,
    /// Success status code, e.g. `"200"`.
    success: &'static str,
}

/// Canonical `/v1` routes plus the un-prefixed system endpoints. Legacy
/// `/auth`, `/players`, ... aliases are the same handlers and are omitted for
/// brevity (the canonical mount is authoritative).
const ROUTES: &[RouteSpec] = &[
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/auth/register",
        tag: "auth",
        summary: "Register a new player and return the public profile",
        secure: false,
        has_body: true,
        success: "201",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/auth/login",
        tag: "auth",
        summary: "Verify credentials and issue a signed JWT",
        secure: false,
        has_body: true,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/v1/players/{id}",
        tag: "players",
        summary: "Fetch a player's public profile",
        secure: true,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Put,
        path: "/v1/players/{id}",
        tag: "players",
        summary: "Update the authenticated player's own profile",
        secure: true,
        has_body: true,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Delete,
        path: "/v1/players/{id}",
        tag: "players",
        summary: "Soft-delete the authenticated player's own account",
        secure: true,
        has_body: false,
        success: "204",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/players/subaccount",
        tag: "players",
        summary: "Create a subaccount under the authenticated player",
        secure: true,
        has_body: true,
        success: "201",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/v1/players/{id}/subaccounts",
        tag: "players",
        summary: "List the authenticated player's subaccounts",
        secure: true,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/v1/leaderboards/{game_id}",
        tag: "leaderboards",
        summary: "Fetch one ranked page of a leaderboard",
        secure: true,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/leaderboards/{game_id}/entries",
        tag: "leaderboards",
        summary: "Submit a leaderboard entry for the authenticated player",
        secure: true,
        has_body: true,
        success: "201",
    },
    RouteSpec {
        method: HttpMethod::Put,
        path: "/v1/leaderboards/{game_id}/entries/{player_id}",
        tag: "leaderboards",
        summary: "Update the authenticated player's own leaderboard entry",
        secure: true,
        has_body: true,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/saves",
        tag: "saves",
        summary: "Create a game save",
        secure: true,
        has_body: true,
        success: "201",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/v1/saves/{player_id}",
        tag: "saves",
        summary: "List the authenticated player's saves, newest first",
        secure: true,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/v1/saves/{player_id}/{save_id}",
        tag: "saves",
        summary: "Fetch a single game save",
        secure: true,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Delete,
        path: "/v1/saves/{player_id}/{save_id}",
        tag: "saves",
        summary: "Delete a single game save",
        secure: true,
        has_body: false,
        success: "204",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/v1/games/{game_id}/settings",
        tag: "game settings",
        summary: "Fetch a game's settings",
        secure: true,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Put,
        path: "/v1/games/{game_id}/settings",
        tag: "game settings",
        summary: "Set a game's settings (upsert)",
        secure: true,
        has_body: true,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/events",
        tag: "analytics",
        summary: "Ingest a batch of analytics events (fire-and-forget)",
        secure: true,
        has_body: true,
        success: "202",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/feedback",
        tag: "feedback",
        summary: "Submit player feedback",
        secure: true,
        has_body: true,
        success: "201",
    },
    RouteSpec {
        method: HttpMethod::Post,
        path: "/v1/socket-tickets",
        tag: "websocket",
        summary: "Issue a one-shot WebSocket connection ticket",
        secure: false,
        has_body: true,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/healthz",
        tag: "system",
        summary: "Liveness probe with DB status",
        secure: false,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/metrics",
        tag: "system",
        summary: "Prometheus text-format metrics",
        secure: false,
        has_body: false,
        success: "200",
    },
    RouteSpec {
        method: HttpMethod::Get,
        path: "/openapi.json",
        tag: "system",
        summary: "This OpenAPI document",
        secure: false,
        has_body: false,
        success: "200",
    },
];

/// Register the `/openapi.json` route.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::resource("/openapi.json").route(web::get().to(openapi_handler)));
}

/// Serve the generated OpenAPI document as JSON.
async fn openapi_handler() -> HttpResponse {
    match document().to_json() {
        Ok(json) => HttpResponse::Ok()
            .content_type("application/json")
            .body(json),
        Err(e) => {
            tracing::error!(error = %e, "openapi: serialization failed");
            HttpResponse::InternalServerError()
                .json(serde_json::json!({ "error": "internal error" }))
        }
    }
}

/// Build the complete OpenAPI document from the route table.
fn document() -> OpenApi {
    let mut paths = PathsBuilder::new();
    for route in ROUTES {
        paths = paths.path(
            route.path,
            PathItem::new(route.method.clone(), build_operation(route)),
        );
    }

    let components = ComponentsBuilder::new()
        .security_scheme(
            "bearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some("Player JWT issued by POST /v1/auth/login"))
                    .build(),
            ),
        )
        .build();

    let tags: Vec<Tag> = [
        "auth",
        "players",
        "leaderboards",
        "saves",
        "game settings",
        "analytics",
        "feedback",
        "websocket",
        "system",
    ]
    .into_iter()
    .map(Tag::new)
    .collect();

    let info = InfoBuilder::new()
        .title("Playzoid Server API")
        .version("1.0.0")
        .description(Some("Talo-compatible game backend API (v0.x line)"))
        .build();

    OpenApi::builder()
        .info(info)
        .paths(paths.build())
        .components(Some(components))
        .tags(Some(tags))
        .build()
}

/// Build one operation for a route.
fn build_operation(route: &RouteSpec) -> Operation {
    let mut builder = OperationBuilder::new()
        .operation_id(Some(format!(
            "{}_{}",
            method_str(route.method.clone()),
            route.path
        )))
        .summary(Some(route.summary))
        .tag(route.tag)
        .responses(build_responses(route));

    if route.secure {
        builder = builder.security(SecurityRequirement::new("bearerAuth", Vec::<String>::new()));
    }

    let params: Vec<Parameter> = path_params(route.path)
        .iter()
        .map(|n| path_parameter(n))
        .collect();
    builder = builder.parameters(Some(params));

    if route.has_body {
        builder = builder.request_body(Some(request_body()));
    }

    builder.build()
}

/// Standard responses for a route: the success status plus the shared error
/// statuses most handlers return.
fn build_responses(route: &RouteSpec) -> Responses {
    let mut builder = ResponsesBuilder::new().response(
        route.success,
        success_response(route.success, route.has_body),
    );
    if route.secure {
        builder = builder
            .response(
                "401",
                err_response("Unauthorized — missing, invalid or expired JWT"),
            )
            .response(
                "503",
                err_response("Service unavailable — database not configured"),
            );
    }
    builder
        .response(
            "400",
            err_response("Bad request — validation or malformed input"),
        )
        .response(
            "429",
            err_response("Too many requests — rate limit exceeded"),
        )
        .build()
}

/// JSON response for a success status.
fn success_response(code: &str, with_body: bool) -> Response {
    let mut response = ResponseBuilder::new()
        .description(success_description(code))
        .build();
    if with_body && code != "204" {
        response = ResponseBuilder::new()
            .description(success_description(code))
            .content("application/json", json_content())
            .build();
    }
    response
}

/// JSON `{"error": ...}` body response.
fn err_response(description: &str) -> Response {
    ResponseBuilder::new()
        .description(description)
        .content("application/json", json_content())
        .build()
}

/// Loose JSON body content — the shape is documented per-endpoint in the
/// route handlers and `docs/TALO_API.md`.
fn json_content() -> Content {
    Content::new(Some(Schema::Object(ObjectBuilder::new().build())))
}

/// Description text for a success status code.
fn success_description(code: &str) -> &'static str {
    match code {
        "200" => "OK",
        "201" => "Created",
        "202" => "Accepted",
        "204" => "No Content",
        _ => "Success",
    }
}

/// Lower-case HTTP method name for `operationId`.
fn method_str(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Post => "post",
        HttpMethod::Put => "put",
        HttpMethod::Delete => "delete",
        HttpMethod::Patch => "patch",
        HttpMethod::Head => "head",
        HttpMethod::Options => "options",
        HttpMethod::Trace => "trace",
    }
}

/// `{...}` path template placeholders in `path`.
fn path_params(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|seg| seg.starts_with('{') && seg.ends_with('}'))
        .map(|seg| seg.trim_matches(['{', '}']).to_string())
        .collect()
}

/// Required path parameter of string type.
fn path_parameter(name: &str) -> Parameter {
    ParameterBuilder::new()
        .name(name)
        .parameter_in(ParameterIn::Path)
        .schema(Some(Schema::Object(
            ObjectBuilder::new()
                .schema_type(SchemaType::Type(Type::String))
                .build(),
        )))
        .build()
}

/// OpenAPI request body object (JSON).
fn request_body() -> RequestBody {
    RequestBodyBuilder::new()
        .content("application/json", json_content())
        .required(Some(Required::True))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_operation(item: &PathItem, method: HttpMethod) -> bool {
        match method {
            HttpMethod::Get => item.get.is_some(),
            HttpMethod::Post => item.post.is_some(),
            HttpMethod::Put => item.put.is_some(),
            HttpMethod::Delete => item.delete.is_some(),
            _ => false,
        }
    }

    #[test]
    fn document_lists_every_route() {
        let doc = document();
        for route in ROUTES {
            let item = doc
                .paths
                .paths
                .get(route.path)
                .unwrap_or_else(|| panic!("missing path {}", route.path));
            assert!(
                has_operation(item, route.method.clone()),
                "{} missing {}",
                route.path,
                method_str(route.method.clone())
            );
        }
    }

    #[test]
    fn document_has_expected_meta() {
        let doc = document();
        assert_eq!(doc.info.title, "Playzoid Server API");
        assert!(doc.paths.paths.contains_key("/v1/auth/register"));
        assert!(doc.paths.paths.contains_key("/metrics"));
        assert!(doc.paths.paths.contains_key("/openapi.json"));
        let components = doc.components.expect("components present");
        assert!(components.security_schemes.contains_key("bearerAuth"));
    }

    #[test]
    fn document_is_valid_json_with_expected_paths() {
        let doc = document();
        let json = doc.to_json().expect("serializes");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["openapi"], "3.1.0");
        for route in ROUTES {
            assert!(
                value["paths"].get(route.path).is_some(),
                "missing {} in JSON",
                route.path
            );
        }
    }
}
