//! Integration tests for movie extension

use zeroclaw_movie::{MovieShowtimesTool, MovieConfig};

#[tokio::test]
async fn test_tool_creation() {
    // Test creating tool without API keys (should work but APIs won't be functional)
    let tool = MovieShowtimesTool::new(None).await;
    assert!(tool.is_ok());
}

#[tokio::test]
async fn test_tool_parameters() {
    let tool = MovieShowtimesTool::new(None).await.unwrap();
    
    let params = tool.parameters();
    assert!(params.is_object());
    
    let props = params.as_object().unwrap().get("properties").unwrap();
    assert!(props.as_object().unwrap().contains_key("movie_name"));
}

#[test]
fn test_config_default() {
    let config = MovieConfig::default();
    assert!(config.enabled);
    assert_eq!(config.defaults.hours_ahead, 3);
}

#[test]
fn test_config_from_env() {
    // Set test environment variables
    std::env::set_var("TMDB_API_KEY", "test_key");
    
    let config = MovieConfig::from_env();
    assert!(config.us.api_key.is_some());
    
    // Clean up
    std::env::remove_var("TMDB_API_KEY");
}
