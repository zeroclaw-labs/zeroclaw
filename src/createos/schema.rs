use powersync::schema::{Column, Schema, Table};

/// Build the PowerSync schema for createOS tables.
///
/// 5 tables mirroring Django createOS models:
/// - createos_lifedomain (11 columns)
/// - createos_epic (17 columns)
/// - createos_sprint (12 columns)
/// - createos_task (24 columns)
/// - createos_userstory (22 columns)
pub fn createos_schema() -> Schema {
    Schema {
        tables: vec![
            lifedomain_table(),
            epic_table(),
            sprint_table(),
            task_table(),
            userstory_table(),
        ],
        raw_tables: vec![],
    }
}

fn lifedomain_table() -> Table {
    Table::create(
        "createos_lifedomain",
        vec![
            // id is implicit in PowerSync (row ID)
            Column::text("name"),
            Column::text("slug"),
            Column::text("description"),
            Column::text("type"),
            Column::text("icon"),
            Column::text("color"),
            Column::integer("is_active"),
            Column::text("notion_id"),
            Column::text("created_at"),
            Column::text("updated_at"),
        ],
        |_| {},
    )
}

fn epic_table() -> Table {
    Table::create(
        "createos_epic",
        vec![
            Column::text("name"),
            Column::text("domain_id"),
            Column::text("status"),
            Column::text("subtitle"),
            Column::text("log_line"),
            Column::text("priority"),
            Column::text("start_date"),
            Column::text("target_date"),
            Column::text("github_repo"),
            Column::text("notion_id"),
            Column::text("budget_tier"),
            Column::real("estimated_budget"),
            Column::text("client"),
            Column::text("production_company"),
            Column::text("created_at"),
            Column::text("updated_at"),
        ],
        |_| {},
    )
}

fn sprint_table() -> Table {
    Table::create(
        "createos_sprint",
        vec![
            Column::text("name"),
            Column::text("domain_id"),
            Column::text("epic_id"),
            Column::text("status"),
            Column::text("objectives"),
            Column::text("start_date"),
            Column::text("end_date"),
            Column::real("quality_score"),
            Column::text("notion_id"),
            Column::text("created_at"),
            Column::text("updated_at"),
        ],
        |_| {},
    )
}

fn task_table() -> Table {
    Table::create(
        "createos_task",
        vec![
            Column::text("title"),
            Column::text("description"),
            Column::text("acceptance_criteria"),
            Column::text("domain_id"),
            Column::text("epic_id"),
            Column::text("sprint_id"),
            Column::text("user_story_id"),
            Column::text("parent_task_id"),
            Column::text("status"),
            Column::text("priority"),
            Column::text("task_type"),
            Column::text("task_category"),
            Column::text("due_date"),
            Column::text("do_date"),
            Column::text("agent_status"),
            Column::text("assigned_agent"),
            Column::text("branch_name"),
            Column::text("pr_url"),
            Column::text("ai_summary"),
            Column::text("note"),
            Column::text("notion_id"),
            Column::text("created_at"),
            Column::text("updated_at"),
        ],
        |_| {},
    )
}

fn userstory_table() -> Table {
    Table::create(
        "createos_userstory",
        vec![
            Column::text("name"),
            Column::text("description"),
            Column::text("acceptance_criteria"),
            Column::text("epic_id"),
            Column::text("sprint_id"),
            Column::text("status"),
            Column::text("user_type"),
            Column::text("priority"),
            Column::integer("story_points"),
            Column::text("notion_id"),
            Column::text("current_interview_round"),
            Column::text("discovery_status"),
            Column::text("interview_transcript"),
            Column::text("personas"),
            Column::text("user_flows"),
            Column::text("edge_cases"),
            Column::text("technical_constraints"),
            Column::text("rbac_requirements"),
            Column::text("research_notes"),
            Column::text("created_at"),
            Column::text("updated_at"),
        ],
        |_| {},
    )
}
