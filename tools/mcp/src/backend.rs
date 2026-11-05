use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, RequestBuilder};
use serde_json::{Value, json};
use url::Url;

/// Caller-scoped backend; never constructed with a service-role credential.
#[derive(Clone)]
pub struct Backend {
    pub client: Client,
    pub base: Url,
    pub key: String,
    pub token: String,
    pub user: Value,
    pub kb: Option<Url>,
    pub georgia: Option<Url>,
}
impl Backend {
    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self.base.join(path)?;
        Ok(self
            .client
            .request(method, url)
            .header("apikey", &self.key)
            .bearer_auth(&self.token))
    }
    pub async fn send(request: RequestBuilder) -> Result<Value> {
        let response = request.send().await.context("Backend request failed")?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .context("Backend response unavailable")?;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !status.is_success() {
            bail!(
                "{}",
                value
                    .get("message")
                    .or_else(|| value.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("Backend request rejected")
            );
        }
        Ok(value)
    }
    pub async fn rpc(&self, name: &str, args: Value) -> Result<Value> {
        Self::send(
            self.request(Method::POST, &format!("/rest/v1/rpc/{name}"))?
                .json(&args),
        )
        .await
    }
    pub async fn admin(&self) -> Result<()> {
        if self.rpc("is_admin", json!({})).await? != true {
            bail!("Admin access required");
        }
        Ok(())
    }
    pub async fn table(
        &self,
        table: &str,
        method: Method,
        data: Option<&Value>,
        query: &[(&str, String)],
        single: bool,
        upsert: bool,
    ) -> Result<Value> {
        let mut request = self
            .request(method, &format!("/rest/v1/{table}"))?
            .query(query)
            .header(
                "Prefer",
                if upsert {
                    "return=representation,resolution=merge-duplicates"
                } else {
                    "return=representation"
                },
            );
        if single {
            request = request.header("Accept", "application/vnd.pgrst.object+json");
        }
        if let Some(data) = data {
            request = request.json(data);
        }
        Self::send(request).await
    }
    pub async fn storage(
        &self,
        method: Method,
        suffix: &str,
        data: Option<Value>,
    ) -> Result<Value> {
        let mut request = self.request(method, &format!("/storage/v1/{suffix}"))?;
        if let Some(data) = data {
            request = request.json(&data);
        }
        Self::send(request).await
    }
    #[must_use]
    pub fn object_path(bucket: &str, path: &str) -> String {
        let mut url = Url::parse("https://storage.invalid/").expect("literal URL");
        url.path_segments_mut()
            .expect("hierarchical URL")
            .extend(std::iter::once(bucket).chain(path.split('/')));
        url.path().trim_start_matches('/').to_owned()
    }
    pub fn public_url(&self, bucket: &str, path: &str) -> Result<String> {
        Ok(self
            .base
            .join(&format!(
                "/storage/v1/object/public/{}",
                Self::object_path(bucket, path)
            ))?
            .to_string())
    }
    pub async fn upload(
        &self,
        bucket: &str,
        path: &str,
        bytes: Vec<u8>,
        mime: &str,
        overwrite: bool,
    ) -> Result<Value> {
        Self::send(
            self.request(
                Method::POST,
                &format!("/storage/v1/object/{}", Self::object_path(bucket, path)),
            )?
            .header("Content-Type", mime)
            .header("x-upsert", overwrite.to_string())
            .body(bytes),
        )
        .await
    }
    pub async fn download(&self, bucket: &str, path: &str) -> Result<Vec<u8>> {
        let response = self
            .request(
                Method::GET,
                &format!("/storage/v1/object/{}", Self::object_path(bucket, path)),
            )?
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("Stored original unavailable");
        }
        // Storage documents are bounded at 25 MiB; refuse unexpected oversized bodies.
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len() + chunk.len() > 26_214_400 {
                bail!("Stored original exceeds 25 MiB");
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    pub async fn remove_object(&self, bucket: &str, path: &str) -> Result<Value> {
        self.storage(
            Method::DELETE,
            &format!("object/{bucket}"),
            Some(json!({"prefixes":[path]})),
        )
        .await
    }
    pub async fn signed_url(
        &self,
        bucket: &str,
        path: &str,
        seconds: u32,
        download: Option<&str>,
    ) -> Result<String> {
        let value = self
            .storage(
                Method::POST,
                &format!("object/sign/{}", Self::object_path(bucket, path)),
                Some(json!({"expiresIn":seconds})),
            )
            .await?;
        let path = value["signedURL"].as_str().context("Missing signed URL")?;
        let mut url = self.base.join(&format!("/storage/v1{path}"))?;
        if let Some(name) = download {
            url.query_pairs_mut().append_pair("download", name);
        }
        Ok(url.to_string())
    }
}
