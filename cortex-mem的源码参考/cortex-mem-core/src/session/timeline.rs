use crate::{CortexFilesystem, FilesystemOperations, Result};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Timeline entry representing a message or event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp: DateTime<Utc>,
    pub uri: String,
    pub summary: String,
    pub role: String,
}

/// Timeline aggregation level
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineAggregation {
    Hourly,
    Daily,
    Monthly,
    Yearly,
}

/// Timeline generator for creating temporal views
pub struct TimelineGenerator {
    filesystem: Arc<CortexFilesystem>,
}

impl TimelineGenerator {
    /// Create a new timeline generator
    pub fn new(filesystem: Arc<CortexFilesystem>) -> Self {
        Self { filesystem }
    }

    /// Generate daily timeline index
    ///
    /// Creates an index file at cortex://session/{thread_id}/timeline/{YYYY-MM}/{DD}/index.md
    pub async fn generate_daily_index(
        &self,
        thread_id: &str,
        year: i32,
        month: u32,
        day: u32,
    ) -> Result<String> {
        let year_month = format!("{:04}-{:02}", year, month);
        let day_str = format!("{:02}", day);
        let timeline_path = format!(
            "cortex://session/{}/timeline/{}/{}",
            thread_id, year_month, day_str
        );

        // List all messages in this day
        let entries = self.filesystem.list(&timeline_path).await?;

        let mut md = String::new();
        md.push_str(&format!("# Timeline: {}-{:02}-{:02}\n\n", year, month, day));
        md.push_str(&format!("**Thread**: {}\n\n", thread_id));
        md.push_str(&format!("**Messages**: {}\n\n", entries.len()));

        md.push_str("## Messages\n\n");

        // Sort entries by name (which contains timestamp)
        let mut message_entries: Vec<_> = entries
            .into_iter()
            .filter(|e| !e.is_directory && e.name.ends_with(".md") && !e.name.starts_with('.'))
            .collect();

        message_entries.sort_by(|a, b| a.name.cmp(&b.name));

        for entry in message_entries {
            // Extract timestamp from filename (HH_MM_SS_id.md)
            let parts: Vec<&str> = entry.name.split('_').collect();
            if parts.len() >= 3 {
                let time = format!("{}:{}:{}", parts[0], parts[1], parts[2]);
                md.push_str(&format!("- [{}]({})\n", time, entry.uri));
            }
        }

        // Save index
        let index_uri = format!("{}/index.md", timeline_path);
        self.filesystem.write(&index_uri, &md).await?;

        Ok(index_uri)
    }

    /// Generate monthly timeline index
    ///
    /// Creates an index file at cortex://session/{thread_id}/timeline/{YYYY-MM}/index.md
    pub async fn generate_monthly_index(
        &self,
        thread_id: &str,
        year: i32,
        month: u32,
    ) -> Result<String> {
        let year_month = format!("{:04}-{:02}", year, month);
        let timeline_path = format!("cortex://session/{}/timeline/{}", thread_id, year_month);

        // List all day directories
        let entries = self.filesystem.list(&timeline_path).await?;

        let mut md = String::new();
        md.push_str(&format!("# Timeline: {}-{:02}\n\n", year, month));
        md.push_str(&format!("**Thread**: {}\n\n", thread_id));

        // Count total messages
        let mut total_messages = 0;
        let mut day_dirs: Vec<_> = entries
            .into_iter()
            .filter(|e| e.is_directory && !e.name.starts_with('.'))
            .collect();

        day_dirs.sort_by(|a, b| a.name.cmp(&b.name));

        md.push_str("## Daily Breakdown\n\n");

        for day_entry in &day_dirs {
            // Count messages in this day
            let day_entries = self.filesystem.list(&day_entry.uri).await?;
            let message_count = day_entries
                .iter()
                .filter(|e| !e.is_directory && e.name.ends_with(".md") && !e.name.starts_with('.'))
                .count();

            total_messages += message_count;

            md.push_str(&format!(
                "- **{}**: {} messages ([view]({}/index.md))\n",
                day_entry.name, message_count, day_entry.uri
            ));
        }

        md.push_str(&format!("\n**Total Messages**: {}\n", total_messages));

        // Save index
        let index_uri = format!("{}/index.md", timeline_path);
        self.filesystem.write(&index_uri, &md).await?;

        Ok(index_uri)
    }

    /// Generate yearly timeline index
    pub async fn generate_yearly_index(&self, thread_id: &str, year: i32) -> Result<String> {
        let timeline_path = format!("cortex://session/{}/timeline", thread_id);

        // List all month directories
        let entries = self.filesystem.list(&timeline_path).await?;

        let mut md = String::new();
        md.push_str(&format!("# Timeline: {}\n\n", year));
        md.push_str(&format!("**Thread**: {}\n\n", thread_id));

        let year_prefix = format!("{:04}-", year);
        let mut month_dirs: Vec<_> = entries
            .into_iter()
            .filter(|e| e.is_directory && e.name.starts_with(&year_prefix))
            .collect();

        month_dirs.sort_by(|a, b| a.name.cmp(&b.name));

        md.push_str("## Monthly Breakdown\n\n");

        for month_entry in &month_dirs {
            // This is a simplified count - in production you'd recursively count
            md.push_str(&format!(
                "- **{}**: ([view]({}/index.md))\n",
                month_entry.name, month_entry.uri
            ));
        }

        // Save index
        let index_uri = format!("{}/{}/index.md", timeline_path, year);
        self.filesystem.write(&index_uri, &md).await?;

        Ok(index_uri)
    }

    /// Generate all timeline indexes for a thread
    pub async fn generate_all_indexes(&self, thread_id: &str) -> Result<Vec<String>> {
        let timeline_path = format!("cortex://session/{}/timeline", thread_id);

        // Check if timeline exists
        if !self.filesystem.exists(&timeline_path).await? {
            return Ok(Vec::new());
        }

        let mut generated = Vec::new();

        // List all year-month directories
        let entries = self.filesystem.list(&timeline_path).await?;

        for year_month_entry in entries {
            if !year_month_entry.is_directory || year_month_entry.name.starts_with('.') {
                continue;
            }

            // Parse year-month (YYYY-MM format)
            let parts: Vec<&str> = year_month_entry.name.split('-').collect();
            if parts.len() != 2 {
                continue;
            }

            let year: i32 = parts[0].parse().unwrap_or(0);
            let month: u32 = parts[1].parse().unwrap_or(0);

            if year == 0 || month == 0 {
                continue;
            }

            // Generate monthly index
            let monthly_index = self.generate_monthly_index(thread_id, year, month).await?;
            generated.push(monthly_index);

            // List day directories and generate daily indexes
            let day_entries = self.filesystem.list(&year_month_entry.uri).await?;

            for day_entry in day_entries {
                if !day_entry.is_directory || day_entry.name.starts_with('.') {
                    continue;
                }

                let day: u32 = day_entry.name.parse().unwrap_or(0);
                if day == 0 {
                    continue;
                }

                let daily_index = self
                    .generate_daily_index(thread_id, year, month, day)
                    .await?;
                generated.push(daily_index);
            }
        }

        Ok(generated)
    }

    /// Get timeline entries for a date range
    pub async fn get_entries(
        &self,
        thread_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<TimelineEntry>> {
        let mut entries = Vec::new();

        // Iterate through each day in the range
        let mut current = start;

        while current <= end {
            let year_month = format!("{:04}-{:02}", current.year(), current.month());
            let day = format!("{:02}", current.day());
            let day_path = format!(
                "cortex://session/{}/timeline/{}/{}",
                thread_id, year_month, day
            );

            // Check if this day exists
            if self.filesystem.exists(&day_path).await? {
                let day_entries = self.filesystem.list(&day_path).await?;

                for entry in day_entries {
                    if !entry.is_directory
                        && entry.name.ends_with(".md")
                        && !entry.name.starts_with('.')
                    {
                        // Create timeline entry (simplified)
                        entries.push(TimelineEntry {
                            timestamp: current,
                            uri: entry.uri,
                            summary: entry.name.clone(),
                            role: "unknown".to_string(),
                        });
                    }
                }
            }

            // Move to next day
            current = current + chrono::Duration::days(1);
        }

        Ok(entries)
    }
}
