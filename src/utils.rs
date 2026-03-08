use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub async fn get_user_id(access_token: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );

    let response = client
        .get("https://api.spotify.com/v1/me")
        .headers(headers)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(response_error("Failed to get user profile", response).await);
    }

    let user_profile: Value = response.json().await?;
    user_profile["id"]
        .as_str()
        .context("User ID not found in profile")
        .map(String::from)
}

pub async fn get_all_items<T: DeserializeOwned>(access_token: &str, url: &str) -> Result<Vec<T>> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );

    let mut items = Vec::new();
    let mut next_url = Some(url.to_string());

    while let Some(url) = next_url {
        let response = client.get(&url).headers(headers.clone()).send().await?;

        if !response.status().is_success() {
            return Err(response_error(
                &format!("Failed to get items from Spotify: {url}"),
                response,
            )
            .await);
        }

        let mut data: Value = response.json().await?;
        let new_items: Vec<T> = serde_json::from_value(data["items"].take())?;
        items.extend(new_items);

        next_url = data["next"].as_str().map(|s| s.to_string());
    }

    Ok(items)
}

pub async fn delete_spotify(access_token: &str, url: &str) -> Result<()> {
    request_spotify(access_token, Method::DELETE, url).await
}

pub async fn request_spotify(access_token: &str, method: Method, url: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );

    let response = client.request(method, url).headers(headers).send().await?;

    if !response.status().is_success() {
        return Err(response_error("Spotify request failed", response).await);
    }

    Ok(())
}

pub async fn response_error(context: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "Could not read response body".to_string());
    anyhow::anyhow!("{context} ({status}): {body}")
}
