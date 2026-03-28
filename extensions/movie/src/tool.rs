//! Movie information tool implementation for ZeroClaw

use crate::api::douban::DoubanApi;
use crate::api::tmdb::TmdbApi;
use crate::api::Movie;
use crate::models::{MovieInfo, MovieListResponse, ToolResult};
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;

fn movie_to_info(m: Movie) -> MovieInfo {
    MovieInfo {
        id: m.id,
        title: m.title,
        original_title: m.original_title,
        rating: m.rating,
        release_date: m.release_date,
        genres: m.genres,
        poster_url: m.poster_url,
        director: m.director,
        cast: m.cast,
        overview: m.overview,
    }
}

/// Movie information query tool
///
/// This tool can be integrated into ZeroClaw as a custom tool.
/// It supports querying currently playing movies in China (via Douban - free)
/// and International (via TMDB - free with registration).
pub struct MovieShowtimesTool {
    china_api: Option<Arc<DoubanApi>>,
    us_api: Option<Arc<TmdbApi>>,
}

impl MovieShowtimesTool {
    /// Create a new movie information tool
    ///
    /// # Arguments
    /// * `tmdb_api_key` - Optional TMDB API key for US/International queries.
    ///                    Get free key at: https://www.themoviedb.org/settings/api
    pub async fn new(tmdb_api_key: Option<String>) -> Result<Self> {
        // Use Douban API (free, no key required)
        let china_api = match DoubanApi::new() {
            Ok(api) => Some(Arc::new(api)),
            Err(e) => {
                log::warn!("Failed to initialize Douban API: {}", e);
                None
            }
        };

        // Use TMDB API (free with registration)
        let us_api = match tmdb_api_key {
            Some(key) if !key.is_empty() => match TmdbApi::new(key) {
                Ok(api) => Some(Arc::new(api)),
                Err(e) => {
                    log::warn!("Failed to initialize TMDB API: {}", e);
                    None
                }
            },
            _ => {
                log::info!("TMDB API key not configured. US region queries will be limited.");
                None
            }
        };

        Ok(Self { china_api, us_api })
    }

    /// Get tool name
    pub fn name(&self) -> &'static str {
        "get_movie_info"
    }

    /// Get tool description
    pub fn description(&self) -> &'static str {
        "Query currently playing movies with basic info and ratings. Supports China (Douban) and International (TMDB)."
    }

    /// Get tool parameters JSON Schema
    pub fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "movie_name": {
                    "type": "string",
                    "description": "Optional movie name to search. If not provided, returns current hot/now-playing movies."
                }
            },
            "required": []
        })
    }

    /// Execute the tool with JSON input
    pub async fn execute(&self, input: Value) -> Result<String> {
        let movie_name = input["movie_name"].as_str().map(|s| s.to_string());
        let result = self.query_movies(movie_name.as_deref()).await?;
        Ok(result.to_string())
    }

    /// Query movies - hot movies or search by name
    ///
    /// Auto-detects region based on query content:
    /// - Contains Chinese characters -> Douban (China)
    /// - Otherwise -> TMDB (International)
    ///
    /// # Arguments
    /// * `movie_name` - Optional search keyword. If None, returns hot/now-playing movies.
    pub async fn query_movies(&self, movie_name: Option<&str>) -> Result<ToolResult> {
        let use_china = movie_name
            .map(|q| q.chars().any(|c| (c as u32) >= 0x4E00 && (c as u32) <= 0x9FFF))
            .unwrap_or(true); // default to China (Douban) if no keyword

        if use_china {
            self.query_china_movies(movie_name).await
        } else {
            self.query_us_movies(movie_name).await
        }
    }

    /// Query movies using China (Douban) API - FREE
    async fn query_china_movies(&self, movie_name: Option<&str>) -> Result<ToolResult> {
        let api = self
            .china_api
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Douban API not initialized"))?;

        let movies = if let Some(name) = movie_name {
            api.search_movies(name, 10).await.unwrap_or_default()
        } else {
            api.get_hot_movies(20).await.unwrap_or_default()
        };

        if movies.is_empty() {
            return Ok(ToolResult::success(
                "查询完成",
                Some(MovieListResponse {
                    source: "豆瓣".to_string(),
                    summary: format!(
                        "未找到相关电影{}",
                        movie_name.map(|m| format!("：{}", m)).unwrap_or_default()
                    ),
                    movies: vec![],
                }),
            ));
        }

        let count = movies.len();
        let summary = movie_name
            .map(|m| format!("搜索「{}」找到 {} 部电影", m, count))
            .unwrap_or_else(|| format!("当前豆瓣热映电影（共 {} 部）", count));

        let movie_infos = movies
            .into_iter()
            .map(movie_to_info)
            .collect();

        Ok(ToolResult::success(
            "查询成功",
            Some(MovieListResponse {
                source: "豆瓣".to_string(),
                summary,
                movies: movie_infos,
            }),
        ))
    }

    /// Query movies using US (TMDB) API - FREE with registration
    async fn query_us_movies(&self, movie_name: Option<&str>) -> Result<ToolResult> {
        let api = self.us_api.as_ref().ok_or_else(|| {
            anyhow::anyhow!("TMDB API not configured. Please set TMDB_API_KEY.")
        })?;

        let mut movies = if let Some(name) = movie_name {
            api.search_movies(name, 1).await.unwrap_or_default()
        } else {
            api.get_now_playing(1).await.unwrap_or_default()
        };

        if movies.is_empty() {
            return Ok(ToolResult::success(
                "Query complete",
                Some(MovieListResponse {
                    source: "TMDB".to_string(),
                    summary: format!(
                        "No movies found{}",
                        movie_name.map(|m| format!(": {}", m)).unwrap_or_default()
                    ),
                    movies: vec![],
                }),
            ));
        }

        // Fetch credits (director + cast) for each movie
        for movie in &mut movies {
            if let Err(e) = api.enrich_movie_credits(movie).await {
                log::warn!("Failed to fetch credits for movie {}: {}", movie.id, e);
            }
        }

        let count = movies.len();
        let summary = movie_name
            .map(|m| format!("Search '{}' found {} movies", m, count))
            .unwrap_or_else(|| format!("Now playing movies ({})", count));

        let movie_infos = movies
            .into_iter()
            .map(movie_to_info)
            .collect();

        Ok(ToolResult::success(
            "Query successful",
            Some(MovieListResponse {
                source: "TMDB".to_string(),
                summary,
                movies: movie_infos,
            }),
        ))
    }
}

// ZeroClaw Tool trait implementation (when compiled with zeroclaw-integration feature)
#[cfg(feature = "zeroclaw-integration")]
impl zeroclaw::tools::traits::Tool for MovieShowtimesTool {
    fn name(&self) -> String {
        self.name().to_string()
    }

    fn description(&self) -> String {
        self.description().to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters()
    }

    async fn execute(&self, input: serde_json::Value) -> zeroclaw::tools::Result<String> {
        self.execute(input)
            .await
            .map_err(|e| zeroclaw::tools::Error::ExecutionFailed(e.to_string()))
    }
}
