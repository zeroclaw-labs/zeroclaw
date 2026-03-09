//! Basic usage example for ZeroClaw Movie Extension
//!
//! Run with:
//!   cargo run --example basic_usage
//!
//! No API key required for China (Douban).
//! For US/International (TMDB), set env var TMDB_API_KEY (free registration at themoviedb.org).

use zeroclaw_movie::MovieShowtimesTool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    println!("ZeroClaw Movie Extension - Demo\n");

    // Create tool — China uses Douban (free, no key needed)
    // US uses TMDB (free with registration, get key at themoviedb.org)
    let tool = MovieShowtimesTool::new(std::env::var("TMDB_API_KEY").ok()).await?;

    println!("Tool initialized.\n");

    // Example 1: Hot movies in China
    println!("--- Example 1: Hot movies in China (Douban) ---");
    let result = tool.query_movies(None).await?;
    println!("{}\n", result);

    // Example 2: Search specific movie
    println!("--- Example 2: Search movie '流浪地球' ---");
    let result2 = tool.query_movies(Some("流浪地球")).await?;
    println!("{}\n", result2);

    // Example 3: International query (requires TMDB API key)
    if std::env::var("TMDB_API_KEY").is_ok() {
        println!("--- Example 3: Now playing in US (TMDB) ---");
        let result3 = tool.query_movies(Some("Dune")).await?;
        println!("{}\n", result3);
    } else {
        println!("--- Example 3: Skipped (set TMDB_API_KEY to enable US queries) ---\n");
    }

    Ok(())
}
