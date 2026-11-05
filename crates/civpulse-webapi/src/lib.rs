//! Authenticated REST entry point. Database RLS remains the authorization authority.
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, Request, State},
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
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            key,
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
}

#[derive(Clone, Copy)]
struct ApiError(StatusCode);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = match self.0 {
            StatusCode::UNAUTHORIZED => "authentication required",
            StatusCode::FORBIDDEN => "permission denied",
            StatusCode::BAD_REQUEST => "invalid election ID or query",
            StatusCode::BAD_GATEWAY => "election results unavailable",
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

/// All routes, fallbacks, and method errors pass through authentication.
pub fn router(backend: Backend) -> Router {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/organizations", get(organizations))
        .route("/api/v1/elections", get(elections))
        .route("/api/v1/people", get(people))
        .route(
            "/api/v1/elections/{election_id}/results",
            get(election_results),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn_with_state(
            backend.clone(),
            authenticate,
        ))
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
