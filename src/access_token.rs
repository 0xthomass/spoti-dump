use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::Deserialize;
use std::{env, time::Duration};
use tiny_http::{Response, Server};
use url::Url;

use crate::Commands;

const REDIRECT_URI: &str = "http://127.0.0.1:8000/callback";

// Request the union of permissions up front so one refresh token can be reused
// across export, import, and purge commands.
const SCOPE_ALL: &str = "user-library-read user-library-modify playlist-read-private playlist-modify-public playlist-modify-private";

#[derive(Deserialize)]
struct AccessTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    scope: Option<String>,
}

pub async fn get_access_token(command: Commands) -> Result<String> {
    dotenv::dotenv().context("Failed to load .env file")?;

    let redirect_uri =
        Url::parse(REDIRECT_URI).expect("Hard-coded redirect URI should always be valid");

    let client_id = env::var("SPOTIFY_CLIENT_ID").context("SPOTIFY_CLIENT_ID not set")?;
    let client_secret =
        env::var("SPOTIFY_CLIENT_SECRET").context("SPOTIFY_CLIENT_SECRET not set")?;

    if let Ok(refresh_token) = env::var("SPOTIFY_REFRESH_TOKEN") {
        if !refresh_token.is_empty() {
            let response = refresh_access_token(&refresh_token, &client_id, &client_secret).await?;

            if token_has_required_scopes(response.scope.as_deref(), &command) {
                return Ok(response.access_token);
            }

            eprintln!(
                "Stored refresh token is missing the scopes required for {}. Re-authorizing in the browser.",
                command_name(&command)
            );
        }
    }

    get_access_token_from_authorization_code(command, &client_id, &client_secret, &redirect_uri)
        .await
}

async fn get_access_token_from_authorization_code(
    command: Commands,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &Url,
) -> Result<String> {
    // Step 1: Get the authorization code
    let (code, _) = get_authorization_code(command, client_id, redirect_uri)?;

    // Step 2: Exchange the code for an access token
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );

    let params = [
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
    ];

    let response = client
        .post("https://accounts.spotify.com/api/token")
        .headers(headers)
        .form(&params)
        .send()
        .await
        .context("Failed to send request")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Could not read response text".to_string());
        anyhow::bail!(
            "Spotify API request failed with status: {}. Response: {}",
            status,
            text
        );
    }

    let response: AccessTokenResponse =
        response.json().await.context("Failed to parse response")?;

    if let Some(refresh_token) = response.refresh_token {
        println!("Your refresh token is: {}", refresh_token);
        println!("Please set it as SPOTIFY_REFRESH_TOKEN in your .env file.");
    }

    Ok(response.access_token)
}

pub async fn get_access_token_from_refresh_token(refresh_token: &str) -> Result<String> {
    dotenv::dotenv().context("Failed to load .env file")?;

    let client_id = env::var("SPOTIFY_CLIENT_ID").context("SPOTIFY_CLIENT_ID not set")?;
    let client_secret =
        env::var("SPOTIFY_CLIENT_SECRET").context("SPOTIFY_CLIENT_SECRET not set")?;

    let response = refresh_access_token(refresh_token, &client_id, &client_secret).await?;
    Ok(response.access_token)
}

async fn refresh_access_token(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<AccessTokenResponse> {
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &client_id),
        ("client_secret", &client_secret),
    ];

    let response = client
        .post("https://accounts.spotify.com/api/token")
        .headers(headers)
        .form(&params)
        .send()
        .await
        .context("Failed to send request")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Could not read response text".to_string());
        anyhow::bail!(
            "Spotify API request failed with status: {}. Response: {}",
            status,
            text
        );
    }

    let response: AccessTokenResponse =
        response.json().await.context("Failed to parse response")?;

    Ok(response)
}

fn token_has_required_scopes(scope: Option<&str>, command: &Commands) -> bool {
    let Some(scope) = scope else {
        return false;
    };

    let granted_scopes: std::collections::HashSet<&str> = scope.split_whitespace().collect();
    required_scopes(command)
        .iter()
        .all(|required_scope| granted_scopes.contains(required_scope))
}

fn required_scopes(command: &Commands) -> &'static [&'static str] {
    match command {
        Commands::Export { .. } => &["user-library-read", "playlist-read-private"],
        Commands::Import { .. } => &[
            "user-library-modify",
            "playlist-modify-public",
            "playlist-modify-private",
        ],
        Commands::Purge { .. } => &[
            "user-library-read",
            "user-library-modify",
            "playlist-read-private",
            "playlist-modify-public",
            "playlist-modify-private",
        ],
    }
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Export { .. } => "`export`",
        Commands::Import { .. } => "`import`",
        Commands::Purge { .. } => "`purge`",
    }
}

fn get_authorization_code(
    _command: Commands,
    client_id: &str,
    redirect_uri: &Url,
) -> Result<(String, String)> {
    // Generate a random state string
    let state: String = {
        let random_bytes: Vec<u8> = (0..16).map(|_| rand::random::<u8>()).collect();
        general_purpose::URL_SAFE_NO_PAD.encode(&random_bytes)
    };

    let scope = SCOPE_ALL;

    let auth_url = Url::parse_with_params(
        "https://accounts.spotify.com/authorize",
        &[
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", &state),
            ("scope", scope),
        ],
    )?;

    let host = redirect_uri
        .host_str()
        .context("Redirect URI must include a host")?;
    let port = redirect_uri
        .port_or_known_default()
        .context("Redirect URI must include a port")?;
    let server_addr = format!("{}:{}", host, port);

    // Open the authorization URL in the user's default browser; fall back to a manual step if it fails.
    match open::that(auth_url.as_str()) {
        Ok(()) => println!("Opened your browser for Spotify authorization."),
        Err(err) => {
            eprintln!(
                "Failed to launch the browser automatically ({err}). Please open the URL manually:"
            );
            println!("{}", auth_url);
        }
    };

    // Start a local server to handle the callback
    let server = Server::http(&server_addr)
        .map_err(|err| anyhow::anyhow!("Failed to start local callback server: {}", err))?;
    println!("Waiting for Spotify authorization... (will time out in 2 minutes)");

    // Wait for the callback with a timeout
    if let Ok(Some(request)) = server.recv_timeout(Duration::from_secs(120)) {
        let callback_url = format!("http://{}:{}{}", host, port, request.url());
        let url = Url::parse(&callback_url)?;
        let code = url
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .context("No code found in callback URL")?;

        let received_state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .context("No state found in callback URL")?;

        if received_state != state {
            return Err(anyhow::anyhow!("State mismatch: CSRF check failed."));
        }

        // Send a response to the browser
        let response =
            Response::from_string("Authorization successful! You can close this window now.");
        request.respond(response)?;

        return Ok((code, state.to_string()));
    }
    Err(anyhow::anyhow!(
        "Authorization timed out. Please try again."
    ))
}

#[cfg(test)]
mod tests {
    use super::{required_scopes, token_has_required_scopes};
    use crate::Commands;

    #[test]
    fn export_requires_read_scopes() {
        let command = Commands::Export { force: false };
        assert!(token_has_required_scopes(
            Some("user-library-read playlist-read-private"),
            &command
        ));
        assert!(!token_has_required_scopes(
            Some("user-library-modify playlist-modify-public playlist-modify-private"),
            &command
        ));
    }

    #[test]
    fn import_requires_modify_scopes() {
        let command = Commands::Import { force: false };
        assert!(token_has_required_scopes(
            Some("user-library-read user-library-modify playlist-read-private playlist-modify-public playlist-modify-private"),
            &command
        ));
        assert!(!token_has_required_scopes(
            Some("user-library-read playlist-read-private"),
            &command
        ));
    }

    #[test]
    fn missing_scope_metadata_is_not_accepted() {
        let command = Commands::Purge { force: false };
        assert_eq!(
            required_scopes(&command),
            &[
                "user-library-read",
                "user-library-modify",
                "playlist-read-private",
                "playlist-modify-public",
                "playlist-modify-private",
            ]
        );
        assert!(!token_has_required_scopes(None, &command));
    }
}
