//! ZeroClaw Movie Extension
//!
//! This crate provides movie information query functionality for ZeroClaw AI agent.
//! It supports querying currently playing movies with ratings and basic info:
//! - China: Douban API (free, no registration required)
//! - International: TMDB API (free with registration)
//!
//! # Example
//!
//! ```rust
//! use zeroclaw_movie::MovieShowtimesTool;
//!
//! #[tokio::main]
//! async fn main() {
//!     // Initialize tool - Douban (China) is free, no key needed
//!     // TMDB (International) requires free registration
//!     let tool = MovieShowtimesTool::new(
//!         std::env::var("TMDB_API_KEY").ok(),
//!     ).await.unwrap();
//!
//!     // Query hot movies
//!     let result = tool.query_movies(None).await.unwrap();
//!     println!("{}", result);
//!
//!     // Search specific movie
//!     let result = tool.query_movies(Some("流浪地球")).await.unwrap();
//!     println!("{}", result);
//! }
//! ```

pub mod api;
mod tool;
mod models;
mod config;

pub use tool::MovieShowtimesTool;
pub use models::*;
pub use config::MovieConfig;
