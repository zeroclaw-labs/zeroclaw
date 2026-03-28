//! Integration tests for knowledge graph client relationship features.

use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use zeroclaw::memory::knowledge_graph::{KnowledgeGraph, NodeType, Relation};
use zeroclaw::tools::knowledge_tool::KnowledgeTool;
use zeroclaw::tools::traits::Tool;

fn test_setup() -> (TempDir, Arc<KnowledgeGraph>, KnowledgeTool) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("knowledge.db");
    let graph = Arc::new(KnowledgeGraph::new(&db_path, 10000).unwrap());
    let tool = KnowledgeTool::new(Arc::clone(&graph));
    (tmp, graph, tool)
}

#[tokio::test]
async fn capture_client_contact_interaction_roundtrip() {
    let (_tmp, _graph, tool) = test_setup();

    // Create client node
    let client_result = tool
        .execute(json!({
            "action": "capture",
            "node_type": "client",
            "title": "Acme Corp",
            "content": "Enterprise client focused on logistics automation",
            "tags": ["enterprise", "logistics"]
        }))
        .await
        .unwrap();
    assert!(client_result.success);
    let client_output: serde_json::Value = serde_json::from_str(&client_result.output).unwrap();
    let client_id = client_output["node_id"].as_str().unwrap();

    // Create contact node
    let contact_result = tool
        .execute(json!({
            "action": "capture",
            "node_type": "contact",
            "title": "Jane Smith",
            "content": "CTO at Acme Corp, technical decision maker",
            "tags": ["technical", "decision-maker"]
        }))
        .await
        .unwrap();
    assert!(contact_result.success);
    let contact_output: serde_json::Value = serde_json::from_str(&contact_result.output).unwrap();
    let contact_id = contact_output["node_id"].as_str().unwrap();

    // Create interaction node
    let interaction_result = tool
        .execute(json!({
            "action": "capture",
            "node_type": "interaction",
            "title": "Discovery call - Q1 2026",
            "content": "Discussed API integration requirements and timeline",
            "tags": ["call", "discovery"]
        }))
        .await
        .unwrap();
    assert!(interaction_result.success);
    let interaction_output: serde_json::Value =
        serde_json::from_str(&interaction_result.output).unwrap();
    let interaction_id = interaction_output["node_id"].as_str().unwrap();

    // Create relationships
    // Contact -> Client (contact_of)
    let relate1 = tool
        .execute(json!({
            "action": "relate",
            "from_id": contact_id,
            "to_id": client_id,
            "relation": "contact_of"
        }))
        .await
        .unwrap();
    assert!(relate1.success);

    // Client -> Interaction (interacted_with)
    let relate2 = tool
        .execute(json!({
            "action": "relate",
            "from_id": client_id,
            "to_id": interaction_id,
            "relation": "interacted_with"
        }))
        .await
        .unwrap();
    assert!(relate2.success);

    // Verify client_network returns all three
    let network_result = tool
        .execute(json!({
            "action": "client_network",
            "client_id": client_id
        }))
        .await
        .unwrap();
    assert!(network_result.success);
    let network_output: serde_json::Value = serde_json::from_str(&network_result.output).unwrap();

    assert_eq!(network_output["client"]["name"], "Acme Corp");
    assert_eq!(network_output["contact_count"], 1);
    assert_eq!(network_output["interaction_count"], 1);
    assert_eq!(network_output["contacts"][0]["name"], "Jane Smith");
    assert_eq!(
        network_output["interactions"][0]["title"],
        "Discovery call - Q1 2026"
    );
}

#[tokio::test]
async fn interaction_log_returns_sorted_recent() {
    let (_tmp, graph, tool) = test_setup();

    // Create client
    let client_id = graph
        .add_node(NodeType::Client, "TestClient", "Test client", &[], None)
        .unwrap();

    // Create 5 interactions with different timestamps
    let mut interaction_ids = Vec::new();
    for i in 1..=5 {
        let id = graph
            .add_node(
                NodeType::Interaction,
                &format!("Interaction {}", i),
                &format!("Content {}", i),
                &[],
                None,
            )
            .unwrap();
        interaction_ids.push(id.clone());

        // Add edge from client to interaction
        graph
            .add_edge(&client_id, &id, Relation::InteractedWith)
            .unwrap();

        // Small delay to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Query interaction log
    let result = tool
        .execute(json!({
            "action": "interaction_log",
            "client_id": client_id
        }))
        .await
        .unwrap();

    assert!(result.success);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["count"], 5);

    // Verify they're sorted by most recent first (Interaction 5 should be first)
    let interactions = output["interactions"].as_array().unwrap();
    assert_eq!(interactions[0]["title"], "Interaction 5");
    assert_eq!(interactions[4]["title"], "Interaction 1");
}

#[tokio::test]
async fn interaction_log_respects_limit() {
    let (_tmp, graph, tool) = test_setup();

    // Create client
    let client_id = graph
        .add_node(NodeType::Client, "TestClient", "Test client", &[], None)
        .unwrap();

    // Create 10 interactions
    for i in 1..=10 {
        let id = graph
            .add_node(
                NodeType::Interaction,
                &format!("Interaction {}", i),
                &format!("Content {}", i),
                &[],
                None,
            )
            .unwrap();
        graph
            .add_edge(&client_id, &id, Relation::InteractedWith)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // Query with limit=3
    let result = tool
        .execute(json!({
            "action": "interaction_log",
            "client_id": client_id,
            "limit": 3
        }))
        .await
        .unwrap();

    assert!(result.success);
    let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(output["count"], 3);
}

#[tokio::test]
async fn client_network_rejects_non_client_node() {
    let (_tmp, graph, tool) = test_setup();

    // Create a Pattern node
    let pattern_id = graph
        .add_node(NodeType::Pattern, "Some Pattern", "Not a client", &[], None)
        .unwrap();

    // Try to call client_network with a non-client node
    let result = tool
        .execute(json!({
            "action": "client_network",
            "client_id": pattern_id
        }))
        .await
        .unwrap();

    assert!(!result.success);
    assert!(result.error.unwrap().contains("not a client"));
}

#[tokio::test]
async fn new_node_types_work_with_existing_search() {
    let (_tmp, graph, tool) = test_setup();

    // Create a Client node with tags
    tool.execute(json!({
        "action": "capture",
        "node_type": "client",
        "title": "SearchTest Corp",
        "content": "Client for testing search functionality",
        "tags": ["test", "search"]
    }))
    .await
    .unwrap();

    // Verify search finds it
    let search_result = tool
        .execute(json!({
            "action": "search",
            "query": "SearchTest"
        }))
        .await
        .unwrap();

    assert!(search_result.success);
    let output: serde_json::Value = serde_json::from_str(&search_result.output).unwrap();
    assert!(output["count"].as_u64().unwrap() > 0);

    // Verify query_by_tags finds it
    let clients = graph.query_by_tags(&["test".to_string()]).unwrap();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].title, "SearchTest Corp");
}

#[tokio::test]
async fn new_relations_work_with_subgraph() {
    let (_tmp, graph, _tool) = test_setup();

    // Create client and interaction
    let client_id = graph
        .add_node(
            NodeType::Client,
            "SubgraphClient",
            "Testing subgraph",
            &[],
            None,
        )
        .unwrap();

    let interaction_id = graph
        .add_node(
            NodeType::Interaction,
            "SubgraphInteraction",
            "Testing subgraph edge",
            &[],
            None,
        )
        .unwrap();

    // Create edge with InteractedWith relation
    graph
        .add_edge(&client_id, &interaction_id, Relation::InteractedWith)
        .unwrap();

    // Verify get_subgraph traverses the new relation
    let (nodes, edges) = graph.get_subgraph(&client_id, 1).unwrap();

    assert_eq!(nodes.len(), 2); // client + interaction
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].relation, Relation::InteractedWith);
}
