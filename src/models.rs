//! Data models for movie queries

use serde::{Deserialize, Serialize};

/// Individual movie information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieInfo {
    pub id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub rating: Option<f32>,
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    pub poster_url: Option<String>,
    pub director: Option<String>,
    pub cast: Vec<String>,
    pub overview: Option<String>,
}

/// Movie list query response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieListResponse {
    /// Data source (e.g., "豆瓣", "TMDB")
    pub source: String,
    /// Summary description
    pub summary: String,
    /// List of movies
    pub movies: Vec<MovieInfo>,
}

/// Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ToolResult {
    #[serde(rename = "success")]
    Success {
        message: String,
        data: Option<MovieListResponse>,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        code: Option<String>,
    },
}

impl ToolResult {
    /// Create a success result
    pub fn success(message: &str, data: Option<MovieListResponse>) -> Self {
        Self::Success {
            message: message.to_string(),
            data,
        }
    }

    /// Create an error result
    pub fn error(message: &str, code: Option<&str>) -> Self {
        Self::Error {
            message: message.to_string(),
            code: code.map(|s| s.to_string()),
        }
    }
}

impl std::fmt::Display for ToolResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolResult::Success { message, data } => {
                writeln!(f, "✅ {}", message)?;
                if let Some(data) = data {
                    writeln!(f, "\n📽️ {} - {}", data.source, data.summary)?;
                    for (i, movie) in data.movies.iter().enumerate() {
                        let rating_str = movie
                            .rating
                            .map(|r| format!(" ⭐{:.1}", r))
                            .unwrap_or_default();
                        let original = movie
                            .original_title
                            .as_ref()
                            .map(|t| format!(" ({})", t))
                            .unwrap_or_default();
                        let date_str = movie
                            .release_date
                            .as_ref()
                            .map(|d| format!(" [{}]", d))
                            .unwrap_or_default();
                        writeln!(
                            f,
                            "\n{}. {}{}{}{}",
                            i + 1,
                            movie.title,
                            original,
                            rating_str,
                            date_str
                        )?;
                        if let Some(director) = &movie.director {
                            writeln!(f, "   导演: {}", director)?;
                        }
                        if !movie.cast.is_empty() {
                            writeln!(f, "   主演: {}", movie.cast.join(", "))?;
                        }
                        if let Some(overview) = &movie.overview {
                            if !overview.is_empty() {
                                let brief = if overview.chars().count() > 80 {
                                    format!("{}...", overview.chars().take(80).collect::<String>())
                                } else {
                                    overview.clone()
                                };
                                writeln!(f, "   简介: {}", brief)?;
                            }
                        }
                    }
                }
                Ok(())
            }
            ToolResult::Error { message, code } => {
                write!(f, "❌ {}", message)?;
                if let Some(code) = code {
                    write!(f, " [{}]", code)?;
                }
                Ok(())
            }
        }
    }
}

impl From<anyhow::Error> for ToolResult {
    fn from(err: anyhow::Error) -> Self {
        ToolResult::error(&err.to_string(), None)
    }
}
