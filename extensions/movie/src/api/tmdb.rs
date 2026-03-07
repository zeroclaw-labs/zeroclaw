//! TMDB (The Movie Database) API implementation for US/International movies
//! 
//! TMDB provides a free API with registration.
//! - Registration: https://www.themoviedb.org/settings/api
//! - Documentation: https://developer.themoviedb.org/docs
//! - Rate limit: 40 requests per 10 seconds
//! - Cost: Free (non-commercial use with attribution)

use super::{CinemaApi, Cinema, Movie, Showtime, MovieDetail};
use anyhow::{Result, Context, bail};
use reqwest::Client;
use serde::Deserialize;
use std::env;

const TMDB_BASE_URL: &str = "https://api.themoviedb.org/3";
const TMDB_IMAGE_BASE: &str = "https://image.tmdb.org/t/p";

/// TMDB API client
pub struct TmdbApi {
    client: Client,
    api_key: String,
    base_url: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbMovieResponse {
    page: i32,
    results: Vec<TmdbMovie>,
    total_pages: i32,
    total_results: i32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbMovie {
    id: i32,
    title: String,
    original_title: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    overview: Option<String>,
    release_date: Option<String>,
    genre_ids: Vec<i32>,
    vote_average: Option<f32>,
    vote_count: Option<i32>,
    popularity: Option<f32>,
    adult: bool,
    video: bool,
    original_language: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbMovieDetail {
    id: i32,
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    release_date: Option<String>,
    runtime: Option<i32>,
    genres: Vec<TmdbGenre>,
    vote_average: Option<f32>,
    vote_count: Option<i32>,
    status: Option<String>,
    tagline: Option<String>,
    budget: Option<i64>,
    revenue: Option<i64>,
    imdb_id: Option<String>,
    homepage: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbGenre {
    id: i32,
    name: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbSearchResponse {
    page: i32,
    results: Vec<TmdbMovie>,
    total_pages: i32,
    total_results: i32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbCreditsResponse {
    id: i32,
    cast: Vec<TmdbCastMember>,
    crew: Vec<TmdbCrewMember>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbCastMember {
    id: i32,
    name: String,
    character: Option<String>,
    order: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TmdbCrewMember {
    id: i32,
    name: String,
    job: Option<String>,
    department: Option<String>,
}

impl TmdbApi {
    /// Create a new TMDB API client
    pub fn new(api_key: String) -> Result<Self> {
        if api_key.is_empty() {
            bail!("TMDB API key cannot be empty");
        }
        
        let client = Client::builder()
            .user_agent("ZeroClaw Movie Bot")
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        
        let base_url = env::var("TMDB_API_URL")
            .unwrap_or_else(|_| TMDB_BASE_URL.to_string());
        
        Ok(Self {
            client,
            api_key,
            base_url,
        })
    }
    
    /// Get now playing movies
    pub async fn get_now_playing(&self, page: u32) -> Result<Vec<Movie>> {
        let url = format!(
            "{}/movie/now_playing?language=en-US&page={}",
            self.base_url, page
        );
        
        log::info!("Fetching now playing movies from TMDB");
        
        let resp = self.client
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .send()
            .await
            .context("Failed to connect to TMDB")?;
        
        if !resp.status().is_success() {
            let error_text = resp.text().await?;
            bail!("TMDB API error: {}", error_text);
        }
        
        let data: TmdbMovieResponse = resp.json().await
            .context("Failed to parse TMDB response")?;
        
        Ok(data.results.into_iter().map(tmdb_to_movie).collect())
    }
    
    /// Get popular movies
    pub async fn get_popular(&self, page: u32) -> Result<Vec<Movie>> {
        let url = format!("{}/movie/popular?language=en-US&page={}", self.base_url, page);
        
        log::info!("Fetching popular movies from TMDB");
        
        let resp = self.client
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .send()
            .await
            .context("Failed to connect to TMDB")?;
        
        if !resp.status().is_success() {
            bail!("TMDB API error: {}", resp.status());
        }
        
        let data: TmdbMovieResponse = resp.json().await
            .context("Failed to parse TMDB response")?;
        
        Ok(data.results.into_iter().map(tmdb_to_movie).collect())
    }
    
    /// Search movies by keyword
    pub async fn search_movies(&self, query: &str, page: u32) -> Result<Vec<Movie>> {
        let url = format!(
            "{}/search/movie?query={}&include_adult=false&language=en-US&page={}",
            self.base_url,
            urlencoding::encode(query),
            page
        );
        
        log::info!("Searching TMDB for: {}", query);
        
        let resp = self.client
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .send()
            .await
            .context("Failed to connect to TMDB")?;
        
        if !resp.status().is_success() {
            bail!("TMDB API error: {}", resp.status());
        }
        
        let data: TmdbSearchResponse = resp.json().await
            .context("Failed to parse TMDB response")?;
        
        Ok(data.results.into_iter().map(tmdb_to_movie).collect())
    }
    
    /// Get movie details by ID
    pub async fn get_movie_detail(&self, movie_id: i32) -> Result<MovieDetail> {
        let url = format!("{}/movie/{}?language=en-US", self.base_url, movie_id);
        
        log::info!("Fetching movie details from TMDB: {}", movie_id);
        
        let resp = self.client
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .send()
            .await
            .context("Failed to connect to TMDB")?;
        
        if !resp.status().is_success() {
            bail!("TMDB API error: {}", resp.status());
        }
        
        let detail: TmdbMovieDetail = resp.json().await
            .context("Failed to parse TMDB response")?;
        
        Ok(MovieDetail {
            id: detail.id.to_string(),
            title: detail.title,
            description: detail.overview,
            director: None, // TMDB requires separate credits endpoint
            cast: vec![],   // TMDB requires separate credits endpoint
            genres: detail.genres.into_iter().map(|g| g.name).collect(),
            duration_minutes: detail.runtime.map(|r| r as u32),
            release_date: detail.release_date,
            rating: detail.vote_average,
            poster_url: detail.poster_path.map(|p| format!("{}original{}", TMDB_IMAGE_BASE, p)),
            trailer_url: detail.homepage,
        })
    }
    
    /// Build image URL from poster path
    pub fn build_image_url(&self, poster_path: &str, size: &str) -> String {
        format!("{}{}{}", TMDB_IMAGE_BASE, size, poster_path)
    }

    /// Fetch credits (director + cast) for a movie and merge into Movie struct
    pub async fn enrich_movie_credits(&self, movie: &mut Movie) -> Result<()> {
        let movie_id: i32 = movie.id.parse().context("Invalid TMDB movie ID")?;
        let url = format!("{}/movie/{}/credits?language=en-US", self.base_url, movie_id);

        let resp = self.client
            .get(&url)
            .query(&[("api_key", &self.api_key)])
            .send()
            .await
            .context("Failed to fetch TMDB credits")?;

        if !resp.status().is_success() {
            return Ok(()); // silently skip on error
        }

        let credits: TmdbCreditsResponse = resp.json().await
            .context("Failed to parse TMDB credits")?;

        // Director: first crew member with job == "Director"
        movie.director = credits.crew.iter()
            .find(|c| c.job.as_deref() == Some("Director"))
            .map(|c| c.name.clone());

        // Cast: top 5 by order
        let mut cast_members = credits.cast;
        cast_members.sort_by_key(|c| c.order.unwrap_or(999));
        movie.cast = cast_members.into_iter()
            .take(5)
            .map(|c| c.name)
            .collect();

        Ok(())
    }
}

fn tmdb_to_movie(m: TmdbMovie) -> Movie {
    Movie {
        id: m.id.to_string(),
        title: m.title,
        original_title: m.original_title,
        poster_url: m.poster_path.map(|p| format!("{}original{}", TMDB_IMAGE_BASE, p)),
        release_date: m.release_date,
        genres: vec![],
        duration_minutes: None,
        rating: m.vote_average,
        overview: m.overview,
        director: None,
        cast: vec![],
    }
}

#[async_trait::async_trait]
impl CinemaApi for TmdbApi {
    fn name(&self) -> &str { "tmdb" }
    fn region(&self) -> &str { "US" }
    
    async fn search_cinemas(&self, _city: &str, _location: Option<&str>) -> Result<Vec<Cinema>> {
        // TMDB does not provide cinema/showtime data
        Ok(vec![])
    }
    
    async fn get_cinema_movies(&self, _cinema_id: &str) -> Result<Vec<Movie>> {
        // Return popular movies as default
        self.get_popular(1).await
    }
    
    async fn get_showtimes(
        &self,
        _cinema_id: &str,
        _movie_id: Option<&str>,
        _hours_ahead: u32,
    ) -> Result<Vec<Showtime>> {
        // TMDB does not provide showtime data
        Ok(vec![])
    }
    
    async fn get_movie_details(&self, movie_id: &str) -> Result<MovieDetail> {
        let id = movie_id.parse::<i32>()
            .context("Invalid TMDB movie ID")?;
        self.get_movie_detail(id).await
    }
}
