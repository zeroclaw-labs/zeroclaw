//! Movie API traits and implementations

pub mod maoyan;
pub mod douban;
pub mod tmdb;
pub mod movieglu;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Cinema API trait - defines the interface for movie data providers
#[async_trait::async_trait]
pub trait CinemaApi: Send + Sync {
    /// Get API name/identifier
    fn name(&self) -> &str;
    
    /// Get supported region/country
    fn region(&self) -> &str;
    
    /// Search cinemas by location
    async fn search_cinemas(&self, city: &str, location: Option<&str>) -> Result<Vec<Cinema>>;
    
    /// Get movies playing at a cinema
    async fn get_cinema_movies(&self, cinema_id: &str) -> Result<Vec<Movie>>;
    
    /// Get showtimes for a specific movie at a cinema
    async fn get_showtimes(
        &self, 
        cinema_id: &str, 
        movie_id: Option<&str>,
        hours_ahead: u32
    ) -> Result<Vec<Showtime>>;
    
    /// Get movie details
    async fn get_movie_details(&self, movie_id: &str) -> Result<MovieDetail>;
}

/// Cinema information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cinema {
    pub id: String,
    pub name: String,
    pub address: String,
    pub city: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub distance_km: Option<f64>,
}

/// Movie basic information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Movie {
    pub id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub poster_url: Option<String>,
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    pub duration_minutes: Option<u32>,
    pub rating: Option<f32>,
    pub overview: Option<String>,
    pub director: Option<String>,
    pub cast: Vec<String>,
}

/// Detailed movie information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieDetail {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub director: Option<String>,
    pub cast: Vec<String>,
    pub genres: Vec<String>,
    pub duration_minutes: Option<u32>,
    pub release_date: Option<String>,
    pub rating: Option<f32>,
    pub poster_url: Option<String>,
    pub trailer_url: Option<String>,
}

/// Showtime information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Showtime {
    pub id: String,
    pub movie_id: String,
    pub movie_title: String,
    pub cinema_id: String,
    pub cinema_name: String,
    pub start_time: String,
    pub end_time: Option<String>,
    pub hall: Option<String>,
    pub language: Option<String>,
    pub version: Option<String>, // e.g., "2D", "3D", "IMAX"
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub available_seats: Option<u32>,
    pub total_seats: Option<u32>,
    pub booking_url: Option<String>,
}

/// Query parameters for showtimes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowtimeQuery {
    pub city: String,
    pub location: Option<String>,
    pub movie_name: Option<String>,
    pub hours_ahead: u32,
    pub date: Option<String>,
}

/// Query result containing all showtimes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShowtimeResult {
    pub query: ShowtimeQuery,
    pub cinemas: Vec<CinemaShowtimes>,
    pub total_count: usize,
}

/// Showtimes grouped by cinema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CinemaShowtimes {
    pub cinema: Cinema,
    pub showtimes: Vec<Showtime>,
}
