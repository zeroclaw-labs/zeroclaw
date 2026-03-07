use crate::security::{policy::ToolOperation, SecurityPolicy};
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const FEISHU_BASE_URL: &str = "https://open.feishu.cn/open-apis";
const LARK_BASE_URL: &str = "https://open.larksuite.com/open-apis";
const TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(120);
const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(7200);
const INVALID_ACCESS_TOKEN_CODE: i64 = 99_991_663;
const MAX_MEDIA_BYTES: usize = 25 * 1024 * 1024; // 25 MiB

const ACTIONS: &[&str] = &[
    "read",
    "write",
    "append",
    "create",
    "list_blocks",
    "get_block",
    "update_block",
    "delete_block",
    "create_table",
    "write_table_cells",
    "create_table_with_values",
    "upload_image",
    "upload_file",
];

#[derive(Debug, Clone)]
struct CachedTenantToken {
    value: String,
    refresh_after: Instant,
}

pub struct FeishuDocTool {
    app_id: String,
    app_secret: String,
    use_feishu: bool,
    security: Arc<SecurityPolicy>,
    tenant_token: Arc<RwLock<Option<CachedTenantToken>>>,
    client: reqwest::Client,
}

impl FeishuDocTool {
    pub fn new(
        app_id: String,
        app_secret: String,
        use_feishu: bool,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            use_feishu,
            security,
            tenant_token: Arc::new(RwLock::new(None)),
            client: crate::config::build_runtime_proxy_client("tool.feishu_doc"),
        }
    }

    fn api_base(&self) -> &str {
        if self.use_feishu {
            FEISHU_BASE_URL
        } else {
            LARK_BASE_URL
        }
    }

    fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    async fn get_tenant_access_token(&self) -> anyhow::Result<String> {
        {
            let cached = self.tenant_token.read().await;
            if let Some(token) = cached.as_ref() {
                if Instant::now() < token.refresh_after {
                    return Ok(token.value.clone());
                }
            }
        }

        let url = format!("{}/auth/v3/tenant_access_token/internal", self.api_base());
        let body = json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret,
        });

        let resp = self.http_client().post(&url).json(&body).send().await?;
        let status = resp.status();
        let payload = parse_json_or_empty(resp).await?;

        if !status.is_success() {
            anyhow::bail!(
                "tenant_access_token request failed: status={}, body={}",
                status,
                sanitize_api_json(&payload)
            );
        }

        ensure_api_success(&payload, "tenant_access_token")?;
        let token = payload
            .get("tenant_access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("tenant_access_token missing from response"))?
            .to_string();

        let ttl_seconds = extract_ttl_seconds(&payload);
        let refresh_after = next_refresh_deadline(Instant::now(), ttl_seconds);

        let mut cached = self.tenant_token.write().await;
        *cached = Some(CachedTenantToken {
            value: token.clone(),
            refresh_after,
        });

        Ok(token)
    }

    async fn invalidate_token(&self) {
        let mut cached = self.tenant_token.write().await;
        *cached = None;
    }

    async fn authed_request(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        self.authed_request_with_query(method, url, body, None)
            .await
    }

    async fn authed_request_with_query(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
        query: Option<&[(&str, String)]>,
    ) -> anyhow::Result<Value> {
        let mut retried = false;

        loop {
            let token = self.get_tenant_access_token().await?;
            let mut req = self
                .http_client()
                .request(method.clone(), url)
                .bearer_auth(token);

            if let Some(q) = query {
                req = req.query(q);
            }
            if let Some(b) = body.clone() {
                req = req.json(&b);
            }

            let resp = req.send().await?;
            let status = resp.status();
            let payload = parse_json_or_empty(resp).await?;

            if should_refresh_token(status, &payload) && !retried {
                retried = true;
                self.invalidate_token().await;
                continue;
            }

            if !status.is_success() {
                anyhow::bail!(
                    "request failed: method={} url={} status={} body={}",
                    method,
                    url,
                    status,
                    sanitize_api_json(&payload)
                );
            }

            ensure_api_success(&payload, "request")?;
            return Ok(payload);
        }
    }

    async fn execute_action(&self, action: &str, args: &Value) -> anyhow::Result<Value> {
        match action {
            "read" => self.action_read(args).await,
            "write" => self.action_write(args).await,
            "append" => self.action_append(args).await,
            "create" => self.action_create(args).await,
            "list_blocks" => self.action_list_blocks(args).await,
            "get_block" => self.action_get_block(args).await,
            "update_block" => self.action_update_block(args).await,
            "delete_block" => self.action_delete_block(args).await,
            "create_table" => self.action_create_table(args).await,
            "write_table_cells" => self.action_write_table_cells(args).await,
            "create_table_with_values" => self.action_create_table_with_values(args).await,
            "upload_image" => self.action_upload_image(args).await,
            "upload_file" => self.action_upload_file(args).await,
            _ => anyhow::bail!(
                "unknown action '{}'. Supported actions: {}",
                action,
                ACTIONS.join(", ")
            ),
        }
    }

    async fn action_read(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let url = format!(
            "{}/docx/v1/documents/{}/raw_content",
            self.api_base(),
            doc_token
        );
        let payload = self.authed_request(Method::GET, &url, None).await?;
        let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));

        Ok(json!({
            "content": data.get("content").cloned().unwrap_or(Value::Null),
            "revision": data.get("revision_id").or_else(|| data.get("revision")).cloned().unwrap_or(Value::Null),
            "title": data.get("title").cloned().unwrap_or(Value::Null),
        }))
    }

    async fn action_write(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let content = required_string(args, "content")?;
        let root_block_id = self.get_root_block_id(&doc_token).await?;

        // Convert first, then delete — prevents data loss if conversion fails
        let converted = self.convert_markdown_blocks(&content).await?;
        if converted.is_empty() {
            anyhow::bail!(
                "markdown conversion produced no blocks — refusing to delete existing content"
            );
        }

        let root_block = self.get_block(&doc_token, &root_block_id).await?;
        let root_children = extract_child_ids(&root_block);
        if !root_children.is_empty() {
            self.batch_delete_children(&doc_token, &root_block_id, 0, root_children.len())
                .await?;
        }

        self.insert_children_blocks(&doc_token, &root_block_id, None, converted.clone())
            .await?;

        Ok(json!({
            "success": true,
            "blocks_written": converted.len(),
        }))
    }

    async fn action_append(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let content = required_string(args, "content")?;
        let root_block_id = self.get_root_block_id(&doc_token).await?;
        let converted = self.convert_markdown_blocks(&content).await?;
        if converted.is_empty() {
            anyhow::bail!(
                "markdown conversion produced no blocks — refusing to append empty content"
            );
        }
        self.insert_children_blocks(&doc_token, &root_block_id, None, converted.clone())
            .await?;

        Ok(json!({
            "success": true,
            "blocks_appended": converted.len(),
        }))
    }

    async fn action_create(&self, args: &Value) -> anyhow::Result<Value> {
        let title = required_string(args, "title")?;
        let folder_token = optional_string(args, "folder_token");
        let owner_open_id = optional_string(args, "owner_open_id");

        let mut create_body = json!({ "title": title });
        if let Some(folder) = &folder_token {
            create_body["folder_token"] = Value::String(folder.clone());
        }

        let create_url = format!("{}/docx/v1/documents", self.api_base());

        // Create the document — single POST, no retry (avoids duplicates)
        let payload = self
            .authed_request(Method::POST, &create_url, Some(create_body.clone()))
            .await?;
        let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));

        let doc_id = first_non_empty_string(&[
            data.get("document").and_then(|v| v.get("document_id")),
            data.get("document").and_then(|v| v.get("document_token")),
            data.get("document_id"),
            data.get("document_token"),
        ])
        .ok_or_else(|| anyhow::anyhow!("create response missing document id"))?;

        // Verify the document exists — retry only the GET, never re-POST
        let verify_url = format!(
            "{}/docx/v1/documents/{}/raw_content",
            self.api_base(),
            doc_id
        );
        let max_verify_attempts = 3usize;
        let mut last_err = String::new();
        for attempt in 1..=max_verify_attempts {
            match self.authed_request(Method::GET, &verify_url, None).await {
                Ok(_) => {
                    // Document verified — proceed with permissions and return
                    let document_url = first_non_empty_string(&[
                        data.get("document").and_then(|v| v.get("url")),
                        data.get("url"),
                    ])
                    .unwrap_or_else(|| {
                        if self.use_feishu {
                            format!("https://feishu.cn/docx/{}", doc_id)
                        } else {
                            format!("https://larksuite.com/docx/{}", doc_id)
                        }
                    });

                    let mut warnings: Vec<String> = Vec::new();
                    if let Some(owner) = &owner_open_id {
                        if let Err(e) = self.grant_owner_permission(&doc_id, owner).await {
                            tracing::warn!(
                                "feishu_doc: document {} created but grant_owner_permission failed: {}",
                                doc_id, e
                            );
                            warnings.push(format!(
                                "Document created but permission grant failed: {}",
                                e
                            ));
                        }
                    }

                    let link_share = args
                        .get("link_share")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if link_share {
                        if let Err(e) = self.enable_link_share(&doc_id).await {
                            tracing::warn!(
                                "feishu_doc: document {} created but link share enable failed: {}",
                                doc_id,
                                e
                            );
                            warnings.push(format!(
                                "Document created but link sharing could not be enabled: {}",
                                e
                            ));
                        }
                    }

                    let mut result = json!({
                        "document_id": doc_id,
                        "title": title,
                        "url": document_url,
                    });
                    if !warnings.is_empty() {
                        result["warning"] = Value::String(warnings.join("; "));
                    }
                    return Ok(result);
                }
                Err(e) => {
                    last_err = format!(
                        "API returned doc_token {} but document not found: {}",
                        doc_id, e
                    );
                    if attempt < max_verify_attempts {
                        tokio::time::sleep(std::time::Duration::from_millis(800 * attempt as u64))
                            .await;
                    }
                }
            }
        }

        anyhow::bail!(
            "document created (id={}) but verification failed after {} attempts: {}",
            doc_id,
            max_verify_attempts,
            last_err
        )
    }

    async fn action_list_blocks(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let blocks = self.list_all_blocks(&doc_token).await?;
        Ok(json!({ "items": blocks }))
    }

    async fn action_get_block(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let block_id = required_string(args, "block_id")?;
        let block = self.get_block(&doc_token, &block_id).await?;
        Ok(json!({ "block": block }))
    }

    async fn action_update_block(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let block_id = required_string(args, "block_id")?;
        let content = required_string(args, "content")?;

        // Convert first, then delete — prevents data loss if conversion fails
        let converted = self.convert_markdown_blocks(&content).await?;
        if converted.is_empty() {
            anyhow::bail!(
                "markdown conversion produced no blocks — refusing to delete existing content"
            );
        }

        let block = self.get_block(&doc_token, &block_id).await?;
        let children = extract_child_ids(&block);
        if !children.is_empty() {
            self.batch_delete_children(&doc_token, &block_id, 0, children.len())
                .await?;
        }

        self.insert_children_blocks(&doc_token, &block_id, None, converted)
            .await?;

        Ok(json!({
            "success": true,
            "block_id": block_id,
        }))
    }

    async fn action_delete_block(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let block_id = required_string(args, "block_id")?;

        let block = self.get_block(&doc_token, &block_id).await?;
        let parent_id =
            first_non_empty_string(&[block.get("parent_id"), block.get("parent_block_id")])
                .ok_or_else(|| anyhow::anyhow!("target block has no parent metadata"))?;

        let parent = self.get_block(&doc_token, &parent_id).await?;
        let children = extract_child_ids(&parent);
        let idx = children
            .iter()
            .position(|id| id == &block_id)
            .ok_or_else(|| anyhow::anyhow!("block not found in parent children list"))?;

        self.batch_delete_children(&doc_token, &parent_id, idx, idx + 1)
            .await?;

        Ok(json!({
            "success": true,
            "block_id": block_id,
        }))
    }

    async fn action_create_table(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let parent = self
            .resolve_parent_block(&doc_token, optional_string(args, "parent_block_id"))
            .await?;
        let row_size = required_usize(args, "row_size")?;
        let column_size = required_usize(args, "column_size")?;
        let column_width = parse_column_width(args)?;

        let mut property = json!({
            "row_size": row_size,
            "column_size": column_size,
        });
        if let Some(widths) = column_width {
            property["column_width"] = Value::Array(widths.into_iter().map(|v| json!(v)).collect());
        }

        let children = vec![json!({
            "block_type": 31,
            "table": {
                "property": property
            }
        })];

        let payload = self
            .insert_children_blocks(&doc_token, &parent, None, children)
            .await?;

        let table_block_id = extract_inserted_block_id(&payload)
            .ok_or_else(|| anyhow::anyhow!("unable to determine created table block id"))?;
        let table_block = self.get_block(&doc_token, &table_block_id).await?;
        let table_cell_block_ids = extract_table_cells(&table_block);

        Ok(json!({
            "success": true,
            "table_block_id": table_block_id,
            "row_size": row_size,
            "column_size": column_size,
            "table_cell_block_ids": table_cell_block_ids,
        }))
    }

    async fn action_write_table_cells(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let table_block_id = required_string(args, "table_block_id")?;
        let values = parse_values_matrix(args)?;

        let table_block = self.get_block(&doc_token, &table_block_id).await?;
        let (row_size, column_size, cell_ids) = extract_table_layout(&table_block)?;

        let mut cells_written = 0usize;
        for (r, row) in values.iter().take(row_size).enumerate() {
            for (c, value) in row.iter().take(column_size).enumerate() {
                let idx = r * column_size + c;
                if idx >= cell_ids.len() {
                    continue;
                }
                self.write_single_cell(&doc_token, &cell_ids[idx], value)
                    .await?;
                cells_written += 1;
            }
        }

        Ok(json!({
            "success": true,
            "table_block_id": table_block_id,
            "cells_written": cells_written,
        }))
    }

    async fn action_create_table_with_values(&self, args: &Value) -> anyhow::Result<Value> {
        let created = self.action_create_table(args).await?;
        let table_block_id = created
            .get("table_block_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("create_table did not return table_block_id"))?;

        let mut write_args = args.clone();
        write_args["table_block_id"] = Value::String(table_block_id.to_string());
        let written = self.action_write_table_cells(&write_args).await?;

        Ok(json!({
            "success": true,
            "table_block_id": table_block_id,
            "cells_written": written.get("cells_written").cloned().unwrap_or_else(|| json!(0)),
            "table_cell_block_ids": created.get("table_cell_block_ids").cloned().unwrap_or_else(|| json!([])),
        }))
    }

    async fn action_upload_image(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let parent = self
            .resolve_parent_block(&doc_token, optional_string(args, "parent_block_id"))
            .await?;
        let index = optional_usize(args, "index")?;
        let filename_override = optional_string(args, "filename");

        let media = self
            .load_media_source(
                optional_string(args, "url"),
                optional_string(args, "file_path"),
                filename_override,
            )
            .await?;

        let uploaded = self
            .upload_media_to_drive(
                &doc_token,
                "docx_image",
                media.filename.as_str(),
                media.bytes,
            )
            .await?;

        let placeholder = format!("![{}](about:blank)", media.filename);
        let converted = self.convert_markdown_blocks(&placeholder).await?;
        if converted.is_empty() {
            anyhow::bail!(
                "image placeholder markdown produced no blocks; cannot insert image block"
            );
        }
        let inserted = self
            .insert_children_blocks(&doc_token, &parent, index, converted)
            .await?;

        let block_id = extract_inserted_block_id(&inserted)
            .ok_or_else(|| anyhow::anyhow!("unable to determine inserted image block id"))?;
        self.patch_image_block(&doc_token, &block_id, &uploaded.file_token)
            .await?;

        Ok(json!({
            "success": true,
            "block_id": block_id,
            "file_token": uploaded.file_token,
        }))
    }

    async fn action_upload_file(&self, args: &Value) -> anyhow::Result<Value> {
        let doc_token = self.resolve_doc_token(args).await?;
        let filename_override = optional_string(args, "filename");

        let media = self
            .load_media_source(
                optional_string(args, "url"),
                optional_string(args, "file_path"),
                filename_override,
            )
            .await?;
        let size = media.bytes.len();
        let uploaded = self
            .upload_media_to_drive(
                &doc_token,
                "docx_file",
                media.filename.as_str(),
                media.bytes,
            )
            .await?;

        Ok(json!({
            "success": true,
            "file_token": uploaded.file_token,
            "file_name": uploaded.file_name,
            "size": size,
        }))
    }

    async fn list_all_blocks(&self, doc_token: &str) -> anyhow::Result<Vec<Value>> {
        const MAX_PAGES: usize = 200;
        let mut items = Vec::new();
        let mut page_token = String::new();
        let mut page_count = 0usize;

        loop {
            page_count += 1;
            if page_count > MAX_PAGES {
                anyhow::bail!(
                    "list_all_blocks exceeded maximum page limit ({}) for document {}",
                    MAX_PAGES,
                    doc_token
                );
            }
            let mut query = vec![("page_size", "500".to_string())];
            if !page_token.is_empty() {
                query.push(("page_token", page_token.clone()));
            }

            let url = format!("{}/docx/v1/documents/{}/blocks", self.api_base(), doc_token);
            let payload = self
                .authed_request_with_query(Method::GET, &url, None, Some(&query))
                .await?;
            let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));

            let page_items = data
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            items.extend(page_items);

            let has_more = data
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_more {
                break;
            }

            page_token = data
                .get("page_token")
                .or_else(|| data.get("next_page_token"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if page_token.is_empty() {
                break;
            }
        }

        Ok(items)
    }

    async fn get_block(&self, doc_token: &str, block_id: &str) -> anyhow::Result<Value> {
        let url = format!(
            "{}/docx/v1/documents/{}/blocks/{}",
            self.api_base(),
            doc_token,
            block_id
        );
        let payload = self.authed_request(Method::GET, &url, None).await?;
        let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));
        Ok(data.get("block").cloned().unwrap_or(data))
    }

    async fn get_root_block_id(&self, doc_token: &str) -> anyhow::Result<String> {
        let blocks = self.list_all_blocks(doc_token).await?;
        if blocks.is_empty() {
            return Ok(doc_token.to_string());
        }

        if let Some(id) = blocks
            .iter()
            .find(|item| {
                item.get("block_id").and_then(Value::as_str) == Some(doc_token)
                    || item
                        .get("parent_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .is_empty()
            })
            .and_then(|item| item.get("block_id").and_then(Value::as_str))
        {
            return Ok(id.to_string());
        }

        blocks
            .first()
            .and_then(|item| item.get("block_id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("unable to determine root block id"))
    }

    async fn convert_markdown_blocks(&self, markdown: &str) -> anyhow::Result<Vec<Value>> {
        let url = format!("{}/docx/v1/documents/blocks/convert", self.api_base());
        let payload = self
            .authed_request(
                Method::POST,
                &url,
                Some(json!({
                    "content_type": "markdown",
                    "content": markdown,
                })),
            )
            .await?;
        let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));

        let first_level_block_ids = data
            .get("first_level_block_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect::<Vec<_>>();

        if let Some(arr) = data.get("blocks").and_then(Value::as_array) {
            if !first_level_block_ids.is_empty() {
                let ordered = first_level_block_ids
                    .iter()
                    .filter_map(|id| {
                        arr.iter()
                            .find(|block| {
                                block.get("block_id").and_then(Value::as_str) == Some(id.as_str())
                            })
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                if !ordered.is_empty() {
                    return Ok(ordered);
                }
            }
            return Ok(arr.clone());
        }

        if !first_level_block_ids.is_empty() {
            if let Some(map) = data.get("blocks").and_then(Value::as_object) {
                let ordered = first_level_block_ids
                    .iter()
                    .filter_map(|id| map.get(id).cloned())
                    .collect::<Vec<_>>();
                if !ordered.is_empty() {
                    return Ok(ordered);
                }
            }
        }

        for key in ["children", "items", "blocks"] {
            if let Some(arr) = data.get(key).and_then(Value::as_array) {
                return Ok(arr.clone());
            }
        }

        if !first_level_block_ids.is_empty() {
            return Ok(first_level_block_ids
                .into_iter()
                .map(|block_id| json!({ "block_id": block_id }))
                .collect());
        }

        Ok(Vec::new())
    }

    async fn insert_children_blocks(
        &self,
        doc_token: &str,
        parent_block_id: &str,
        index: Option<usize>,
        children: Vec<Value>,
    ) -> anyhow::Result<Value> {
        let url = format!(
            "{}/docx/v1/documents/{}/blocks/{}/children",
            self.api_base(),
            doc_token,
            parent_block_id
        );

        let mut body = json!({ "children": children });
        if let Some(i) = index {
            body["index"] = json!(i);
        }

        self.authed_request(Method::POST, &url, Some(body)).await
    }

    async fn batch_delete_children(
        &self,
        doc_token: &str,
        parent_block_id: &str,
        start_index: usize,
        end_index: usize,
    ) -> anyhow::Result<Value> {
        let url = format!(
            "{}/docx/v1/documents/{}/blocks/{}/children/batch_delete",
            self.api_base(),
            doc_token,
            parent_block_id
        );
        let body = json!({
            "start_index": start_index,
            "end_index": end_index,
        });
        let query = [("document_revision_id", "-1".to_string())];
        self.authed_request_with_query(Method::DELETE, &url, Some(body), Some(&query))
            .await
    }

    async fn grant_owner_permission(
        &self,
        document_id: &str,
        owner_open_id: &str,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/drive/v1/permissions/{}/members",
            self.api_base(),
            document_id
        );
        let body = json!({
            "member_type": "openid",
            "member_id": owner_open_id,
            "perm": "full_access",
            "perm_type": "container",
            "type": "user"
        });
        let query = [("type", "docx".to_string())];
        let _ = self
            .authed_request_with_query(Method::POST, &url, Some(body), Some(&query))
            .await?;
        Ok(())
    }

    async fn enable_link_share(&self, document_id: &str) -> anyhow::Result<()> {
        let url = format!(
            "{}/drive/v2/permissions/{}/public",
            self.api_base(),
            document_id
        );
        let body = json!({
            "link_share_entity": "anyone_readable",
            "external_access_entity": "open"
        });
        let query = [("type", "docx".to_string())];
        let _ = self
            .authed_request_with_query(Method::PATCH, &url, Some(body), Some(&query))
            .await;
        Ok(())
    }

    async fn resolve_wiki_token(&self, node_token: &str) -> anyhow::Result<String> {
        let url = format!("{}/wiki/v2/spaces/get_node", self.api_base());
        let query = [("token", node_token.to_string())];
        let payload = self
            .authed_request_with_query(Method::GET, &url, None, Some(&query))
            .await?;
        let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));
        let node = data.get("node").cloned().unwrap_or_else(|| json!({}));
        let obj_token = node
            .get("obj_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("wiki node response missing obj_token"))?;
        Ok(obj_token.to_string())
    }

    async fn resolve_doc_token(&self, args: &Value) -> anyhow::Result<String> {
        let raw_token = required_string(args, "doc_token")?;
        let is_wiki = args
            .get("is_wiki")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_wiki {
            return self.resolve_wiki_token(&raw_token).await;
        }
        Ok(raw_token)
    }

    async fn resolve_parent_block(
        &self,
        doc_token: &str,
        parent_block_id: Option<String>,
    ) -> anyhow::Result<String> {
        match parent_block_id {
            Some(id) => Ok(id),
            None => self.get_root_block_id(doc_token).await,
        }
    }

    async fn write_single_cell(
        &self,
        doc_token: &str,
        cell_block_id: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        // Convert first, then delete — prevents data loss if conversion fails
        let converted = self.convert_markdown_blocks(value).await?;
        if converted.is_empty() {
            anyhow::bail!(
                "markdown conversion produced no blocks — refusing to delete existing cell content"
            );
        }

        let cell_block = self.get_block(doc_token, cell_block_id).await?;
        let children = extract_child_ids(&cell_block);
        if !children.is_empty() {
            self.batch_delete_children(doc_token, cell_block_id, 0, children.len())
                .await?;
        }

        let _ = self
            .insert_children_blocks(doc_token, cell_block_id, None, converted)
            .await?;
        Ok(())
    }

    async fn load_media_source(
        &self,
        url: Option<String>,
        file_path: Option<String>,
        filename_override: Option<String>,
    ) -> anyhow::Result<LoadedMedia> {
        match (url, file_path) {
            (Some(u), None) => self.download_media(&u, filename_override).await,
            (None, Some(p)) => self.read_local_media(&p, filename_override).await,
            (Some(_), Some(_)) => anyhow::bail!("provide only one of 'url' or 'file_path'"),
            (None, None) => anyhow::bail!("either 'url' or 'file_path' is required"),
        }
    }

    async fn download_media(
        &self,
        url: &str,
        filename_override: Option<String>,
    ) -> anyhow::Result<LoadedMedia> {
        // SSRF protection: validate URL scheme and block local/private hosts
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| anyhow::anyhow!("invalid media URL '{}': {}", url, e))?;
        match parsed.scheme() {
            "http" | "https" => {}
            other => anyhow::bail!(
                "unsupported URL scheme '{}': only http/https allowed",
                other
            ),
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("media URL has no host: {}", url))?;
        if crate::tools::url_validation::is_private_or_local_host(host) {
            anyhow::bail!("Blocked local/private host in media URL: {}", host);
        }

        // Use a no-redirect client to prevent SSRF bypass via HTTP redirects
        // (an attacker could redirect to internal/private IPs after initial URL validation)
        let no_redirect_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build no-redirect HTTP client: {}", e))?;
        let mut resp = no_redirect_client.get(url).send().await?;
        let status = resp.status();
        if status.is_redirection() {
            anyhow::bail!(
                "media URL returned a redirect ({}); redirects are not allowed for security",
                status
            );
        }
        if let Some(len) = resp.content_length() {
            if len > MAX_MEDIA_BYTES as u64 {
                anyhow::bail!(
                    "remote media too large: {} bytes (max {} bytes)",
                    len,
                    MAX_MEDIA_BYTES
                );
            }
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "failed downloading url: status={} body={}",
                status,
                crate::providers::sanitize_api_error(&body)
            );
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_MEDIA_BYTES {
                anyhow::bail!(
                    "remote media too large after download: {} bytes (max {} bytes)",
                    bytes.len(),
                    MAX_MEDIA_BYTES
                );
            }
        }

        let guessed = filename_from_url(url).unwrap_or_else(|| "upload.bin".to_string());
        let filename = filename_override.unwrap_or(guessed);
        Ok(LoadedMedia { bytes, filename })
    }

    async fn read_local_media(
        &self,
        file_path: &str,
        filename_override: Option<String>,
    ) -> anyhow::Result<LoadedMedia> {
        if !self.security.is_path_allowed(file_path) {
            anyhow::bail!("Path not allowed by security policy: {}", file_path);
        }

        let resolved = resolve_workspace_path(&self.security.workspace_dir, file_path)?;
        if !self.security.is_resolved_path_allowed(&resolved) {
            anyhow::bail!(self.security.resolved_path_violation_message(&resolved));
        }

        let metadata = tokio::fs::metadata(&resolved).await?;
        if metadata.len() > MAX_MEDIA_BYTES as u64 {
            anyhow::bail!(
                "local media too large: {} bytes (max {} bytes)",
                metadata.len(),
                MAX_MEDIA_BYTES
            );
        }
        let bytes = tokio::fs::read(&resolved).await?;
        if bytes.len() > MAX_MEDIA_BYTES {
            anyhow::bail!(
                "local media too large after read: {} bytes (max {} bytes)",
                bytes.len(),
                MAX_MEDIA_BYTES
            );
        }
        let fallback = resolved
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("upload.bin")
            .to_string();
        let filename = filename_override.unwrap_or(fallback);
        Ok(LoadedMedia { bytes, filename })
    }

    async fn upload_media_to_drive(
        &self,
        doc_token: &str,
        parent_type: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<UploadedMedia> {
        let url = format!("{}/drive/v1/medias/upload_all", self.api_base());
        let mut retried = false;

        loop {
            let token = self.get_tenant_access_token().await?;
            let form = reqwest::multipart::Form::new()
                .text("file_name", filename.to_string())
                .text("parent_type", parent_type.to_string())
                .text("parent_node", doc_token.to_string())
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(bytes.clone()).file_name(filename.to_string()),
                );

            let resp = self
                .http_client()
                .post(&url)
                .bearer_auth(token)
                .multipart(form)
                .send()
                .await?;

            let status = resp.status();
            let payload = parse_json_or_empty(resp).await?;

            if should_refresh_token(status, &payload) && !retried {
                retried = true;
                self.invalidate_token().await;
                continue;
            }

            if !status.is_success() {
                anyhow::bail!(
                    "media upload failed: status={} body={}",
                    status,
                    sanitize_api_json(&payload)
                );
            }
            ensure_api_success(&payload, "media upload")?;

            let data = payload.get("data").cloned().unwrap_or_else(|| json!({}));
            let file_token =
                first_non_empty_string(&[data.get("file_token"), data.get("token")])
                    .ok_or_else(|| anyhow::anyhow!("upload response missing file_token"))?;
            let file_name = first_non_empty_string(&[data.get("name"), data.get("file_name")])
                .unwrap_or_else(|| filename.to_string());
            return Ok(UploadedMedia {
                file_token,
                file_name,
            });
        }
    }

    async fn patch_image_block(
        &self,
        doc_token: &str,
        block_id: &str,
        file_token: &str,
    ) -> anyhow::Result<()> {
        let url = format!(
            "{}/docx/v1/documents/{}/blocks/{}",
            self.api_base(),
            doc_token,
            block_id
        );
        let body = json!({
            "replace_image": {
                "token": file_token
            },
            "block": {
                "block_type": 27,
                "image": {
                    "token": file_token
                }
            }
        });
        let _ = self.authed_request(Method::PATCH, &url, Some(body)).await?;
        Ok(())
    }
}

#[async_trait]
impl Tool for FeishuDocTool {
    fn name(&self) -> &str {
        "feishu_doc"
    }

    fn description(&self) -> &str {
        "Feishu document operations. Actions: read, write, append, create, list_blocks, get_block, update_block, delete_block, create_table, write_table_cells, create_table_with_values, upload_image, upload_file.\n\nIMPORTANT RULES:\n1. After any create, write, append, or update_block action, ALWAYS share the document URL with the user IN THE SAME REPLY. Format: https://feishu.cn/docx/{doc_token} — Do not say 'I will send it later', do not wait for the user to ask.\n2. When outputting Feishu document URLs, use PLAIN TEXT only. Do NOT wrap URLs in Markdown formatting such as **url**, [text](url), or `url`. Feishu messages are plain text and Markdown symbols like ** will be included in the parsed URL, breaking the link.\n3. NEVER fabricate or guess a doc_token from memory. If you do not have the token from the current conversation or from memory_store, tell the user: 'The token has been lost, the document needs to be recreated.' A wrong token causes 404 errors, which is worse than admitting you don't know.\n4. Rule 3 applies to ALL tool calls that return one-time identifiers, not just feishu_doc."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTIONS,
                    "description": "Operation to run"
                },
                "doc_token": {
                    "type": "string",
                    "description": "Document token (required for most actions)"
                },
                "is_wiki": {
                    "type": "boolean",
                    "description": "Set to true if doc_token is a wiki node_token that needs resolution to the actual document token"
                },
                "content": {
                    "type": "string",
                    "description": "Markdown content for write/append/update_block"
                },
                "title": {
                    "type": "string",
                    "description": "Document title for create"
                },
                "folder_token": {
                    "type": "string",
                    "description": "Target folder token for create"
                },
                "owner_open_id": {
                    "type": "string",
                    "description": "Owner open_id to grant full_access after creation"
                },
                "link_share": {
                    "type": "boolean",
                    "description": "Enable link sharing after create (default: false). Set true to make the document link-readable."
                },
                "block_id": {
                    "type": "string",
                    "description": "Block ID for get_block/update_block/delete_block"
                },
                "parent_block_id": {
                    "type": "string",
                    "description": "Optional parent block for create_table/upload_image/upload_file"
                },
                "row_size": {
                    "type": "integer",
                    "description": "Table row count for create_table/create_table_with_values"
                },
                "column_size": {
                    "type": "integer",
                    "description": "Table column count for create_table/create_table_with_values"
                },
                "column_width": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Optional column widths in px"
                },
                "table_block_id": {
                    "type": "string",
                    "description": "Table block ID for write_table_cells"
                },
                "values": {
                    "type": "array",
                    "items": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "description": "2D string matrix for table cell values"
                },
                "url": {
                    "type": "string",
                    "description": "Remote URL for upload_image/upload_file"
                },
                "file_path": {
                    "type": "string",
                    "description": "Local file path for upload_image/upload_file"
                },
                "filename": {
                    "type": "string",
                    "description": "Optional override filename"
                },
                "index": {
                    "type": "integer",
                    "description": "Optional insertion index for upload_image"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'action' parameter".to_string()),
                });
            }
        };

        let operation = match action {
            "read" | "list_blocks" | "get_block" => ToolOperation::Read,
            _ => ToolOperation::Act,
        };
        if let Err(e) = self
            .security
            .enforce_tool_operation(operation, "feishu_doc")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
            });
        }

        match self.execute_action(action, &args).await {
            Ok(result) => Ok(ToolResult {
                success: true,
                output: result.to_string(),
                error: None,
            }),
            Err(err) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(crate::providers::sanitize_api_error(&err.to_string())),
            }),
        }
    }
}

#[derive(Debug)]
struct LoadedMedia {
    bytes: Vec<u8>,
    filename: String,
}

#[derive(Debug)]
struct UploadedMedia {
    file_token: String,
    file_name: String,
}

fn parse_column_width(args: &Value) -> anyhow::Result<Option<Vec<usize>>> {
    let Some(widths) = args.get("column_width") else {
        return Ok(None);
    };

    let arr = widths
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("'column_width' must be an array of integers"))?;

    let parsed = arr
        .iter()
        .map(|v| {
            let n = v.as_u64().ok_or_else(|| {
                anyhow::anyhow!("column_width entries must be non-negative integers")
            })?;
            usize::try_from(n).map_err(|_| anyhow::anyhow!("column_width value too large"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Some(parsed))
}

fn parse_values_matrix(args: &Value) -> anyhow::Result<Vec<Vec<String>>> {
    let values = args
        .get("values")
        .ok_or_else(|| anyhow::anyhow!("Missing 'values' parameter"))?
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("'values' must be an array of arrays of strings"))?;

    values
        .iter()
        .map(|row| {
            let cols = row
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("each row in 'values' must be an array"))?;
            cols.iter()
                .map(|cell| {
                    cell.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| anyhow::anyhow!("table cell values must be strings"))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .collect::<anyhow::Result<Vec<_>>>()
}

fn extract_table_layout(block: &Value) -> anyhow::Result<(usize, usize, Vec<String>)> {
    let row_size = block
        .get("table")
        .and_then(|v| v.get("property"))
        .and_then(|v| v.get("row_size"))
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .or_else(|| {
            block
                .get("table")
                .and_then(|v| v.get("row_size"))
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("table block missing row_size metadata"))?;

    let column_size = block
        .get("table")
        .and_then(|v| v.get("property"))
        .and_then(|v| v.get("column_size"))
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .or_else(|| {
            block
                .get("table")
                .and_then(|v| v.get("column_size"))
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("table block missing column_size metadata"))?;

    let cells = extract_table_cells(block);
    Ok((row_size, column_size, cells))
}

fn extract_table_cells(block: &Value) -> Vec<String> {
    block
        .get("table")
        .and_then(|v| v.get("cells"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn extract_child_ids(block: &Value) -> Vec<String> {
    block
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn extract_inserted_block_id(payload: &Value) -> Option<String> {
    let data = payload.get("data")?;
    for candidate in [
        data.get("children")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first()),
        data.get("items")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first()),
        data.get("block").into_iter().next(),
    ] {
        if let Some(id) = candidate
            .and_then(|v| v.get("block_id"))
            .and_then(Value::as_str)
        {
            return Some(id.to_string());
        }
    }
    None
}

fn required_string(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Missing '{}' parameter", key))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn required_usize(args: &Value, key: &str) -> anyhow::Result<usize> {
    let raw = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("Missing '{}' parameter", key))?;
    usize::try_from(raw).map_err(|_| anyhow::anyhow!("'{}' value is too large", key))
}

fn optional_usize(args: &Value, key: &str) -> anyhow::Result<Option<usize>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let raw = v
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("'{}' must be a non-negative integer", key))?;
            let parsed = usize::try_from(raw)
                .map_err(|_| anyhow::anyhow!("'{}' value {} is too large", key, raw))?;
            Ok(Some(parsed))
        }
    }
}

async fn parse_json_or_empty(resp: reqwest::Response) -> anyhow::Result<Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    serde_json::from_str::<Value>(&body).map_err(|e| {
        anyhow::anyhow!(
            "invalid JSON response: status={} error={} body={}",
            status,
            e,
            crate::providers::sanitize_api_error(&body)
        )
    })
}

fn sanitize_api_json(body: &Value) -> String {
    crate::providers::sanitize_api_error(&body.to_string())
}

fn ensure_api_success(body: &Value, context: &str) -> anyhow::Result<()> {
    let code = body
        .get("code")
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} failed: response missing 'code' field body={}",
                context,
                sanitize_api_json(body)
            )
        })?
        .as_i64()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} failed: response 'code' is not an integer body={}",
                context,
                sanitize_api_json(body)
            )
        })?;
    if code == 0 {
        return Ok(());
    }

    let msg = body
        .get("msg")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("unknown api error");

    anyhow::bail!(
        "{} failed: code={} msg={} body={}",
        context,
        code,
        msg,
        sanitize_api_json(body)
    )
}

fn should_refresh_token(status: reqwest::StatusCode, body: &Value) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED
        || body.get("code").and_then(Value::as_i64) == Some(INVALID_ACCESS_TOKEN_CODE)
}

fn extract_ttl_seconds(body: &Value) -> u64 {
    body.get("expire")
        .or_else(|| body.get("expires_in"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TOKEN_TTL.as_secs())
        .max(1)
}

fn next_refresh_deadline(now: Instant, ttl_seconds: u64) -> Instant {
    let ttl = Duration::from_secs(ttl_seconds.max(1));
    let refresh_in = ttl
        .checked_sub(TOKEN_REFRESH_SKEW)
        .unwrap_or(Duration::from_secs(1));
    now + refresh_in
}

fn first_non_empty_string(values: &[Option<&Value>]) -> Option<String> {
    values.iter().find_map(|candidate| {
        candidate
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}

fn filename_from_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let mut segments = parsed.path_segments()?;
    let tail = segments.next_back()?.trim();
    if tail.is_empty() {
        None
    } else {
        Some(tail.to_string())
    }
}

fn resolve_workspace_path(workspace_dir: &Path, path: &str) -> anyhow::Result<PathBuf> {
    let raw = PathBuf::from(path);
    let joined = if raw.is_absolute() {
        raw
    } else {
        workspace_dir.join(raw)
    };

    joined
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("failed to resolve file path: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> FeishuDocTool {
        FeishuDocTool::new(
            "app_id".to_string(),
            "app_secret".to_string(),
            true,
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[test]
    fn test_parameters_schema_is_valid_json_schema() {
        let schema = tool().parameters_schema();
        assert_eq!(
            schema.get("type"),
            Some(&Value::String("object".to_string()))
        );
        assert!(schema.get("properties").is_some());

        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(required.contains(&Value::String("action".to_string())));
    }

    #[test]
    fn test_name_and_description() {
        let t = tool();
        assert_eq!(t.name(), "feishu_doc");

        let description = t.description();
        for action in ACTIONS {
            assert!(description.contains(action));
        }
    }

    #[tokio::test]
    async fn test_action_dispatch_unknown_action() {
        let t = tool();
        let result = t
            .execute(json!({ "action": "unknown_action" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("unknown action"));
    }

    #[tokio::test]
    async fn test_action_dispatch_missing_doc_token() {
        let t = tool();
        let result = t.execute(json!({ "action": "read" })).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("Missing 'doc_token' parameter"));
    }

    #[test]
    fn test_extract_ttl_seconds_defaults_and_clamps() {
        assert_eq!(extract_ttl_seconds(&json!({"expire": 3600})), 3600);
        assert_eq!(extract_ttl_seconds(&json!({"expires_in": 1800})), 1800);
        // Missing key falls back to DEFAULT_TOKEN_TTL
        assert_eq!(extract_ttl_seconds(&json!({})), DEFAULT_TOKEN_TTL.as_secs());
        // Zero is clamped to 1
        assert_eq!(extract_ttl_seconds(&json!({"expire": 0})), 1);
    }

    #[tokio::test]
    async fn test_write_rejects_empty_conversion() {
        let t = tool();
        // Provide a doc_token and content that is whitespace-only.
        // Since the tool cannot reach the API, convert_markdown_blocks will fail
        // or return empty, and we verify the tool does not succeed silently.
        let result = t
            .execute(json!({ "action": "write", "doc_token": "fake_token", "content": "" }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn test_api_base_feishu_vs_lark() {
        let feishu_tool = FeishuDocTool::new(
            "a".to_string(),
            "b".to_string(),
            true,
            Arc::new(SecurityPolicy::default()),
        );
        assert_eq!(feishu_tool.api_base(), FEISHU_BASE_URL);

        let lark_tool = FeishuDocTool::new(
            "a".to_string(),
            "b".to_string(),
            false,
            Arc::new(SecurityPolicy::default()),
        );
        assert_eq!(lark_tool.api_base(), LARK_BASE_URL);
    }
}
