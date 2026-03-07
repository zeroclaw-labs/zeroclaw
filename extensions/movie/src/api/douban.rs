//! Douban Movie API implementation for China movie info
//! 
//! Uses Douban's official web API endpoints (no API key required).
//! These endpoints are used by Douban's own website.
//!
//! Available endpoints:
//! - Search by tag: https://movie.douban.com/j/search_subjects
//! - Search suggestion: https://movie.douban.com/j/subject_suggest
//!
//! Note: No real-time showtime or cinema data is available.
//! For production use, consider a paid ticketing API (e.g., juhe.cn).

use super::{CinemaApi, Cinema, Movie, Showtime, MovieDetail};
use anyhow::{Result, Context, bail};
use reqwest::Client;
use serde::Deserialize;
use std::env;

const DOUBAN_BASE_URL: &str = "https://movie.douban.com";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Douban Movie API client (free, no key required)
pub struct DoubanApi {
    client: Client,
    base_url: String,
}

// Response from /j/search_subjects
#[derive(Debug, Deserialize)]
struct SearchSubjectsResp {
    subjects: Vec<SubjectItem>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SubjectItem {
    id: String,
    title: String,
    rate: Option<String>,
    cover: Option<String>,
    url: Option<String>,
    is_new: Option<bool>,
}

// Response from /j/subject_suggest
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SuggestItem {
    id: String,
    title: String,
    sub_title: Option<String>,
    img: Option<String>,
    url: Option<String>,
    year: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
}

impl DoubanApi {
    /// Create a new Douban API client (no API key needed)
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        
        let base_url = env::var("DOUBAN_API_URL")
            .unwrap_or_else(|_| DOUBAN_BASE_URL.to_string());
        
        Ok(Self { client, base_url })
    }
    
    /// Get hot/trending movies
    pub async fn get_hot_movies(&self, count: u32) -> Result<Vec<Movie>> {
        let url = format!(
            "{}/j/search_subjects?type=movie&tag=%E7%83%AD%E9%97%A8&page_limit={}&page_start=0",
            self.base_url, count
        );
        
        log::info!("Fetching hot movies from Douban: {}", url);
        
        let resp = self.client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to Douban")?;
        
        if !resp.status().is_success() {
            bail!("Douban API error: {}", resp.status());
        }
        
        let data: SearchSubjectsResp = resp.json().await
            .context("Failed to parse Douban response")?;
        
        Ok(data.subjects.into_iter().map(subject_to_movie).collect())
    }
    
    /// Search movies by keyword
    pub async fn search_movies(&self, keyword: &str, count: u32) -> Result<Vec<Movie>> {
        let encoded = urlencoding::encode(keyword);
        let url = format!(
            "{}/j/subject_suggest?q={}",
            self.base_url, encoded
        );
        
        log::info!("Searching Douban for: {}", keyword);
        
        let resp = self.client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to Douban")?;
        
        if !resp.status().is_success() {
            bail!("Douban API error: {}", resp.status());
        }
        
        let items: Vec<SuggestItem> = resp.json().await
            .context("Failed to parse Douban search response")?;
        
        let movies = items.into_iter()
            .filter(|i| i.item_type.as_deref() == Some("movie"))
            .take(count as usize)
            .map(|i| Movie {
                id: i.id,
                title: i.title,
                original_title: i.sub_title,
                poster_url: i.img,
                release_date: i.year.map(|y| format!("{}-01-01", y)),
                genres: vec![],
                duration_minutes: None,
                rating: None,
                overview: None,
                director: None,
                cast: vec![],
            })
            .collect();
        
        Ok(movies)
    }
}

fn subject_to_movie(s: SubjectItem) -> Movie {
    Movie {
        id: s.id,
        title: s.title,
        original_title: None,
        poster_url: s.cover,
        release_date: None,
        genres: vec![],
        duration_minutes: None,
        rating: s.rate.and_then(|r| r.parse::<f32>().ok()),
        overview: None,
        director: None,
        cast: vec![],
    }
}

#[async_trait::async_trait]
impl CinemaApi for DoubanApi {
    fn name(&self) -> &str { "douban" }
    fn region(&self) -> &str { "CN" }
    
    async fn search_cinemas(&self, _city: &str, _location: Option<&str>) -> Result<Vec<Cinema>> {
        // Douban does not provide cinema/showtime data
        Ok(vec![])
    }
    
    async fn get_cinema_movies(&self, _cinema_id: &str) -> Result<Vec<Movie>> {
        self.get_hot_movies(20).await
    }
    
    async fn get_showtimes(
        &self,
        _cinema_id: &str,
        _movie_id: Option<&str>,
        _hours_ahead: u32,
    ) -> Result<Vec<Showtime>> {
        // Douban does not provide showtime data
        Ok(vec![])
    }
    
    async fn get_movie_details(&self, movie_id: &str) -> Result<MovieDetail> {
        bail!("Douban web API does not provide movie detail endpoint (id: {})", movie_id)
    }
}
