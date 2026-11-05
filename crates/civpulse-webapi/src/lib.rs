//! Authenticated REST entry point. Database RLS remains the authorization authority.
mod openapi;

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct Backend {
    client: reqwest::Client,
    base: reqwest::Url,
    key: HeaderValue,
    kb: Option<reqwest::Url>,
    georgia: Option<reqwest::Url>,
}

impl Backend {
    /// Construct a backend with a publishable key, never a service-role key.
    ///
    /// # Errors
    /// Rejects non-HTTPS origins (except loopback for tests) and privileged keys.
    pub fn new(origin: &str, publishable_key: &str) -> Result<Self> {
        let base = reqwest::Url::parse(origin).context("invalid SUPABASE_URL")?;
        ensure!(
            (base.scheme() == "https"
                || (base.scheme() == "http"
                    && matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
                && base.host_str().is_some()
                && base.username().is_empty()
                && base.password().is_none()
                && base.path() == "/"
                && base.query().is_none()
                && base.fragment().is_none(),
            "SUPABASE_URL must be an HTTPS origin"
        );
        ensure!(
            publishable_key.starts_with("sb_publishable_"),
            "SUPABASE_PUBLISHABLE_KEY must be a publishable key"
        );
        let mut key = HeaderValue::from_str(publishable_key)?;
        key.set_sensitive(true);
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .connect_timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            key,
            kb: std::env::var("KB_API_URL")
                .ok()
                .map(|s| reqwest::Url::parse(&s))
                .transpose()?,
            georgia: std::env::var("GEORGIA_SOURCE_READER_URL")
                .ok()
                .map(|s| reqwest::Url::parse(&s))
                .transpose()?,
        })
    }

    fn request(&self, path: &str, bearer: &HeaderValue) -> reqwest::RequestBuilder {
        // Paths are compile-time constants, never caller-selected URLs.
        let mut url = self.base.clone();
        url.set_path(path);
        self.client
            .get(url)
            .header("apikey", self.key.clone())
            .header(header::AUTHORIZATION, bearer.clone())
    }
}

#[derive(Clone)]
struct Identity {
    bearer: HeaderValue,
    user: User,
}

#[derive(Clone, Deserialize, Serialize)]
struct User {
    id: String,
    #[serde(default)]
    is_anonymous: bool,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Clone, Copy)]
struct ApiError(StatusCode);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match self.0 {
            StatusCode::UNAUTHORIZED => "authentication required",
            StatusCode::FORBIDDEN => "permission denied",
            StatusCode::BAD_REQUEST => "invalid request or arguments",
            StatusCode::BAD_GATEWAY => "data service unavailable",
            StatusCode::NOT_FOUND => "record or operation unavailable",
            StatusCode::CONFLICT => "record conflict",
            StatusCode::UNPROCESSABLE_ENTITY => "invalid record values",
            _ => "identity or data service unavailable",
        };
        let mut response = (self.0, Json(json!({"error": message}))).into_response();
        if self.0 == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

/// Data routes, fallbacks, and method errors require authentication; documentation is public.
pub fn router(backend: Backend) -> Router {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/operations", get(operation_catalog))
        .route(
            "/api/v1/operations/{operation}",
            get(operation_schema).post(invoke_operation),
        )
        .route("/api/v1/civic-resources", get(civic_resources))
        .route(
            "/api/v1/data/{resource}",
            get(search_civic)
                .post(create_civic)
                .patch(update_civic)
                .delete(delete_civic),
        )
        .route("/api/v1/data/{resource}/{id}", get(read_civic))
        .route("/api/v1/organizations", get(organizations))
        .route("/api/v1/elections", get(elections))
        .route("/api/v1/people", get(people))
        .route(
            "/api/v1/elections/{election_id}/results",
            get(election_results),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(DefaultBodyLimit::max(40 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            backend.clone(),
            authenticate,
        ))
        .route("/api/v1/openapi.json", get(openapi::document))
        .route("/api/v1/docs", get(openapi::ui))
        .route("/api/v1/docs/", get(openapi::ui))
        .layer(middleware::from_fn(no_store))
        .with_state(backend)
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn authenticate(
    State(backend): State<Backend>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
    let mut bearer = values
        .next()
        .cloned()
        .ok_or(ApiError(StatusCode::UNAUTHORIZED))?;
    if values.next().is_some() {
        return Err(ApiError(StatusCode::UNAUTHORIZED));
    }
    let text = bearer
        .to_str()
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED))?;
    let (scheme, token) = text
        .split_once(' ')
        .ok_or(ApiError(StatusCode::UNAUTHORIZED))?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || token.len() > 8192
        || token.chars().any(char::is_whitespace)
    {
        return Err(ApiError(StatusCode::UNAUTHORIZED));
    }
    bearer.set_sensitive(true);
    let response = backend
        .request("/auth/v1/user", &bearer)
        .send()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err(ApiError(StatusCode::UNAUTHORIZED));
    }
    if !response.status().is_success() {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE));
    }
    let user: User = response
        .json()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?;
    if user.id.is_empty() || user.is_anonymous {
        return Err(ApiError(StatusCode::UNAUTHORIZED));
    }
    request.extensions_mut().insert(Identity { bearer, user });
    Ok(next.run(request).await)
}

async fn me(Extension(identity): Extension<Identity>) -> Json<User> {
    Json(identity.user)
}

async fn organizations(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
) -> Result<Json<Value>, ApiError> {
    let response = backend
        .request("/rest/v1/organizations", &identity.bearer)
        .query(&[
            ("select", "id,slug,name"),
            ("order", "id.asc"),
            ("limit", "100"),
        ])
        .send()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?;
    match response.status() {
        StatusCode::UNAUTHORIZED => return Err(ApiError(StatusCode::UNAUTHORIZED)),
        StatusCode::FORBIDDEN => return Err(ApiError(StatusCode::FORBIDDEN)),
        status if !status.is_success() => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE)),
        _ => {}
    }
    Ok(Json(
        response
            .json()
            .await
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?,
    ))
}

#[derive(Default, Deserialize)]
struct ResultsQuery {
    #[serde(default)]
    query: String,
}

async fn election_results(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path(election_id): Path<String>,
    Query(query): Query<ResultsQuery>,
) -> Result<Json<Value>, ApiError> {
    if uuid::Uuid::parse_str(&election_id).is_err() || query.query.len() > 200 {
        return Err(ApiError(StatusCode::BAD_REQUEST));
    }
    civpulse_election_results::read(
        &backend.client,
        &backend.base,
        &backend.key,
        &identity.bearer,
        &election_id,
        &query.query,
    )
    .await
    .map(Json)
    .map_err(|_| ApiError(StatusCode::BAD_GATEWAY))
}

async fn elections(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<ResultsQuery>,
) -> Result<Json<Value>, ApiError> {
    search_table(
        &backend,
        &identity,
        "elections",
        "name",
        "id,slug,name,election_date,results_json_url",
        &query.query,
    )
    .await
}
async fn people(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Query(query): Query<ResultsQuery>,
) -> Result<Json<Value>, ApiError> {
    search_table(
        &backend,
        &identity,
        "people",
        "full_name",
        "id,slug,full_name",
        &query.query,
    )
    .await
}
async fn search_table(
    backend: &Backend,
    identity: &Identity,
    table: &str,
    field: &str,
    select: &str,
    query: &str,
) -> Result<Json<Value>, ApiError> {
    if query.len() > 200 {
        return Err(ApiError(StatusCode::BAD_REQUEST));
    }
    let mut request = backend
        .request(&format!("/rest/v1/{table}"), &identity.bearer)
        .query(&[("select", select), ("order", "id.asc"), ("limit", "100")]);
    if !query.trim().is_empty() {
        request = request.query(&[(field, format!("ilike.%{}%", query.trim()))]);
    }
    let response = request
        .send()
        .await
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?;
    if !response.status().is_success() {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE));
    }
    Ok(Json(
        response
            .json()
            .await
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?,
    ))
}

async fn operation_catalog() -> Json<Value> {
    Json(json!({"operations":civpulse_civic::catalog::tools()}))
}
async fn civic_resources() -> Json<Value> {
    Json(civpulse_civic::resources::manifest().clone())
}
async fn operation_schema(Path(operation): Path<String>) -> Result<Json<Value>, ApiError> {
    civpulse_civic::catalog::tools()
        .iter()
        .find(|t| t["name"] == operation)
        .cloned()
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND))
}
async fn execute(
    backend: &Backend,
    identity: &Identity,
    operation: &str,
    args: Value,
) -> Result<Json<Value>, ApiError> {
    if !civpulse_civic::catalog::tools()
        .iter()
        .any(|t| t["name"] == operation)
    {
        return Err(ApiError(StatusCode::NOT_FOUND));
    }
    let authorization = identity
        .bearer
        .to_str()
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED))?;
    let (_, token) = authorization
        .split_once(' ')
        .ok_or(ApiError(StatusCode::UNAUTHORIZED))?;
    let scoped = civpulse_civic::backend::Backend {
        client: backend.client.clone(),
        base: backend.base.clone(),
        key: backend
            .key
            .to_str()
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?
            .to_owned(),
        token: token.to_owned(),
        user: serde_json::to_value(&identity.user)
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE))?,
        kb: backend.kb.clone(),
        georgia: backend.georgia.clone(),
    };
    civpulse_civic::handlers::call(&scoped, operation, args)
        .await
        .map(Json)
        .map_err(|error| {
            let status = if let Some(backend) =
                error.downcast_ref::<civpulse_civic::backend::BackendError>()
            {
                match backend.status {
                    400 => StatusCode::BAD_REQUEST,
                    401 => StatusCode::UNAUTHORIZED,
                    403 => StatusCode::FORBIDDEN,
                    404 => StatusCode::NOT_FOUND,
                    409 => StatusCode::CONFLICT,
                    422 => StatusCode::UNPROCESSABLE_ENTITY,
                    _ => StatusCode::BAD_GATEWAY,
                }
            } else if error.downcast_ref::<reqwest::Error>().is_some() {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::BAD_REQUEST
            };
            ApiError(status)
        })
}
async fn invoke_operation(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path(operation): Path<String>,
    Json(args): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    execute(&backend, &identity, &operation, args).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CivicSearch {
    #[serde(default)]
    query: String,
    #[serde(default)]
    filters: Option<String>,
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}
fn default_limit() -> u32 {
    20
}
async fn search_civic(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path(resource): Path<String>,
    Query(query): Query<CivicSearch>,
) -> Result<Json<Value>, ApiError> {
    let filters = query
        .filters
        .map_or(Ok(json!({})), |s| serde_json::from_str(&s))
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST))?;
    execute(&backend,&identity,"search_civic_data",json!({"resource":resource,"query":query.query,"filters":filters,"limit":query.limit,"offset":query.offset})).await
}
async fn read_civic(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path((resource, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let r = civpulse_civic::resources::manifest()["resources"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["name"] == resource))
        .ok_or(ApiError(StatusCode::NOT_FOUND))?;
    let pk = r["primary_key"]
        .as_array()
        .ok_or(ApiError(StatusCode::BAD_REQUEST))?;
    if pk.len() != 1 {
        return Err(ApiError(StatusCode::BAD_REQUEST));
    }
    let name = pk[0].as_str().ok_or(ApiError(StatusCode::BAD_REQUEST))?;
    execute(
        &backend,
        &identity,
        "get_civic_data",
        json!({"resource":resource,"keys":{name:id}}),
    )
    .await
}
async fn create_civic(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path(resource): Path<String>,
    Json(data): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    execute(
        &backend,
        &identity,
        "create_civic_data",
        json!({"resource":resource,"data":data}),
    )
    .await
}
async fn update_civic(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path(resource): Path<String>,
    Json(mut args): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let object = args
        .as_object_mut()
        .ok_or(ApiError(StatusCode::BAD_REQUEST))?;
    if object.contains_key("resource") {
        return Err(ApiError(StatusCode::BAD_REQUEST));
    }
    object.insert("resource".into(), json!(resource));
    execute(&backend, &identity, "update_civic_data", args).await
}
async fn delete_civic(
    State(backend): State<Backend>,
    Extension(identity): Extension<Identity>,
    Path(resource): Path<String>,
    Json(mut args): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let object = args
        .as_object_mut()
        .ok_or(ApiError(StatusCode::BAD_REQUEST))?;
    if object.contains_key("resource") {
        return Err(ApiError(StatusCode::BAD_REQUEST));
    }
    object.insert("resource".into(), json!(resource));
    execute(&backend, &identity, "delete_civic_data", args).await
}
