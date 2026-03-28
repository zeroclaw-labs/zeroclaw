# How-To: Download Google Sheets for Offline Analysis

This guide explains how to configure ZeroClaw to download Google Sheets as CSV files for local analysis.

## Prerequisites

- ZeroClaw installed and configured.
- A Google Sheet with "Anyone with the link can view" permissions (or public).
- The `http_request` tool enabled in your configuration.

## 1. Configure Allowed Domains

By default, ZeroClaw restricts HTTP requests to specific domains for security. You must allow `docs.google.com` to download sheets.

1. Open your `config.toml` (usually at `~/.zeroclaw/config.toml`).
2. Add or update the `[http_request]` section:

```toml
[http_request]
allowed_domains = ["docs.google.com"]
```

## 2. Format the Download URL

To download a Google Sheet as a CSV, you must modify the standard "edit" URL to an "export" URL.

**Original URL:**
`https://docs.google.com/spreadsheets/d/SPREADSHEET_ID/edit#gid=SHEET_ID`

**Export URL:**
`https://docs.google.com/spreadsheets/d/SPREADSHEET_ID/export?format=csv&gid=SHEET_ID`

> **Note:** If you omit `&gid=SHEET_ID`, it will download the first sheet in the document.

## 3. Execute the Task

You can now ask ZeroClaw to download and analyze the sheet.

**Prompt:**
> "Please download this Google Sheet as a CSV for offline analysis: https://docs.google.com/spreadsheets/d/1La4FNw8tM3nHVcwoG-D0YmAm2LRWdZtH8e4pXt011Uo/export?format=csv. Save it to `analysis_data.csv` and then give me a summary of the columns."

### What ZeroClaw does:
1. **Validates the Domain**: Checks if `docs.google.com` is in the allowed list.
2. **Performs HTTP GET**: Uses the `http_request` tool to fetch the CSV content.
3. **Saves File**: Uses `file_write` to persist the data to your workspace.
4. **Analyzes**: Uses `file_read` to inspect the content and provide the summary.

## Troubleshooting

- **403 Forbidden**: Ensure the Google Sheet is shared as "Anyone with the link can view". ZeroClaw's standard `http_request` tool does not handle Google OAuth headers automatically.
- **Domain Blocked**: Double-check that `allowed_domains` in `config.toml` includes `docs.google.com` exactly.
- **Large Files**: If the sheet is extremely large, you may need to increase `max_response_size` in the `[http_request]` section of your config.
