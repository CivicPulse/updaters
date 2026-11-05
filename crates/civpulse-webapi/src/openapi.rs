//! Public documentation; API operations still require a verified user token.
use axum::{Json, response::Html};
use serde_json::{Value, json};

pub async fn document() -> Json<Value> {
    Json(spec())
}

fn parameter(name: &str, location: &str, schema: Value) -> Value {
    let mut value = json!({"name":name,"in":location,"required":location == "path"});
    value["schema"] = schema;
    value
}

fn operation(summary: &str) -> Value {
    let mut responses = json!({"200":{"description":"Successful response","content":{"application/json":{"schema":{}}}}});
    for code in ["400", "401", "403", "404", "409", "422", "502", "503"] {
        responses[code] = json!({"description":match code {
            "400" => "Invalid request or arguments", "401" => "Authentication required",
            "403" => "Permission denied", "404" => "Record or operation unavailable",
            "409" => "Record conflict", "422" => "Invalid record values",
            "502" => "Data service unavailable", _ => "Identity or data service unavailable"
        },"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}});
    }
    json!({"summary":summary,"responses":responses})
}

fn body(op: &mut Value, schema: Value) {
    op["requestBody"] = json!({"required":true,"content":{"application/json":{}}});
    op["requestBody"]["content"]["application/json"]["schema"] = schema;
}

fn spec() -> Value {
    let mut paths = json!({});
    for (path, summary) in [
        ("/api/v1/me", "Get the verified user"),
        (
            "/api/v1/operations",
            "List operations and their input schemas",
        ),
        (
            "/api/v1/civic-resources",
            "List civic resources and their fields",
        ),
        (
            "/api/v1/organizations",
            "List up to 100 visible organizations",
        ),
        ("/api/v1/elections", "Search up to 100 visible elections"),
        ("/api/v1/people", "Search up to 100 visible people"),
        (
            "/api/v1/elections/{election_id}/results",
            "Read election results",
        ),
        (
            "/api/v1/operations/{operation}",
            "Get an operation and its input schema",
        ),
        ("/api/v1/data/{resource}", "Search civic records"),
        (
            "/api/v1/data/{resource}/{id}",
            "Read a civic record with a single-column primary key",
        ),
    ] {
        paths[path]["get"] = operation(summary);
        let mut parameters = Vec::new();
        for name in ["resource", "id", "operation", "election_id"] {
            if path.contains(&format!("{{{name}}}")) {
                let schema = if name == "election_id" {
                    json!({"type":"string","format":"uuid"})
                } else {
                    json!({"type":"string"})
                };
                parameters.push(parameter(name, "path", schema));
            }
        }
        if !parameters.is_empty() {
            paths[path]["parameters"] = json!(parameters);
        }
    }
    for path in [
        "/api/v1/elections",
        "/api/v1/people",
        "/api/v1/elections/{election_id}/results",
    ] {
        paths[path]["get"]["parameters"] = json!([parameter(
            "query",
            "query",
            json!({"type":"string","default":"","maxLength":200})
        )]);
    }
    paths["/api/v1/data/{resource}"]["get"]["parameters"] = json!([
        parameter("query", "query", json!({"type":"string","default":""})),
        parameter(
            "filters",
            "query",
            json!({"type":"string","description":"JSON-encoded field/value filters","default":"{}"})
        ),
        parameter(
            "limit",
            "query",
            json!({"type":"integer","minimum":0,"default":20})
        ),
        parameter(
            "offset",
            "query",
            json!({"type":"integer","minimum":0,"default":0})
        )
    ]);
    let mut invoke = operation("Invoke a catalog operation");
    invoke["description"] = json!(
        "Request arguments follow the inputSchema returned by GET /api/v1/operations/{operation}."
    );
    body(
        &mut invoke,
        json!({"type":"object","additionalProperties":true}),
    );
    paths["/api/v1/operations/{operation}"]["post"] = invoke;
    add_crud(&mut paths);
    json!({
        "openapi":"3.1.0",
        "info":{"title":"Civic Pulse REST API","version":env!("CARGO_PKG_VERSION"),"description":"Authenticated civic data access. Use a non-anonymous Supabase user access token. Database grants and RLS apply to every operation. Responses vary by resource and operation; use the discovery endpoints for their schemas."},
        "servers":[{"url":"/"}],
        "security":[{"bearerAuth":[]}],
        "paths":paths,
        "components":{"securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer","bearerFormat":"JWT"}},"schemas":{"Error":{"type":"object","required":["error"],"properties":{"error":{"type":"string"}}}}}
    })
}

pub async fn ui() -> Html<&'static str> {
    Html(include_str!("swagger.html"))
}

fn add_crud(paths: &mut Value) {
    for (method, name, summary) in [
        ("post", "create_civic_data", "Create a civic record"),
        ("patch", "update_civic_data", "Update a civic record"),
        ("delete", "delete_civic_data", "Delete a civic record"),
    ] {
        let mut op = operation(summary);
        let schema = if method == "post" {
            json!({"type":"object","additionalProperties":true,"description":"Record fields; see the civic resource inventory."})
        } else {
            let mut schema = civpulse_civic::catalog::tools()
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("CRUD operation exists in catalog")["inputSchema"]
                .clone();
            if let Some(properties) = schema["properties"].as_object_mut() {
                properties.remove("resource");
            }
            if let Some(required) = schema["required"].as_array_mut() {
                required.retain(|field| field != "resource");
            }
            schema
        };
        body(&mut op, schema);
        paths["/api/v1/data/{resource}"][method] = op;
    }
}
