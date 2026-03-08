use spoti_dump::access_token;
use spoti_dump::import;
use std::env;

async fn get_test_access_token() -> String {
    dotenv::dotenv().ok();
    let refresh_token = env::var("SPOTIFY_REFRESH_TOKEN");
    if refresh_token.is_err() {
        panic!("Skipping test: SPOTIFY_REFRESH_TOKEN not set.");
    }
    let refresh_token = refresh_token.unwrap();
    access_token::get_access_token_from_refresh_token(&refresh_token)
        .await
        .unwrap()
}

#[tokio::test]
async fn test_import_saved_tracks_dry_run() {
    let access_token = get_test_access_token().await;
    let result = import::saved_tracks::import_saved_tracks(&access_token, false).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_import_playlists_dry_run() {
    let access_token = get_test_access_token().await;
    let result = import::playlists::import_playlists(&access_token, false).await;
    assert!(result.is_ok());
}
