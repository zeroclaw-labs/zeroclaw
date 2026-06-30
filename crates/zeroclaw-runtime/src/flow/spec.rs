use crate::flow::config_write::{WriteError, WriteTarget, write_response, write_to_target};
use crate::flow::transport::{FlowTransport, Outcome, Prompt};
use crate::response_type::ResponseValue;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use zeroclaw_config::schema::Config;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Node(NodeId),
    Terminal(Outcome),
}

pub type EdgeChoice = Result<(), ()>;

pub struct Node {
    pub id: NodeId,
    pub layer: String,
    pub instance: String,
    pub prop: String,
    pub optional: bool,
    pub prompt: Prompt,
    pub on_success: Step,
    pub on_failure: Step,
    /// Value-keyed edges taken before `on_success` when the response validates.
    /// The first branch whose `ResponseValue` equals the response wins; an empty
    /// list keeps the node a plain linear/binary step. Lets a `Choice` node route
    /// each option to its own next node, so a decision (e.g. peer-group
    /// create-new vs attach vs skip) is a real spec node rather than an ad-hoc
    /// prompt.
    pub branches: Vec<(ResponseValue, Step)>,
    /// Side-effect run before the response write: ensure a map-keyed section
    /// entry exists (`create_map_key(section, key)`), so a node can write into a
    /// freshly created group/alias. Idempotent; `None` for plain field nodes.
    pub ensure_map_key: Option<(String, String)>,
    /// Where the validated response is written. `None` writes the config prop at
    /// `prop` (the default). `Some` routes through `write_to_target`, letting a
    /// node write a personality file into the agent workspace instead of config.
    pub write_target: Option<WriteTarget>,
    pub validate: Box<dyn Fn(&ResponseValue) -> EdgeChoice + Send + Sync>,
}

impl Node {
    #[must_use]
    pub fn display_label(&self) -> String {
        format!(
            "{}:{} ask ({})",
            self.layer,
            self.instance,
            self.prompt.response_type.ask_kind()
        )
    }

    #[must_use]
    pub fn resolve(&self, response: &ResponseValue) -> Step {
        match (self.validate)(response) {
            Ok(()) => self
                .branches
                .iter()
                .find(|(value, _)| value == response)
                .map(|(_, step)| step.clone())
                .unwrap_or_else(|| self.on_success.clone()),
            Err(()) => self.on_failure.clone(),
        }
    }
}

fn response_is_empty(response: &ResponseValue) -> bool {
    match response {
        ResponseValue::Secret(secret) => secret.expose().is_empty(),
        ResponseValue::FreeformText(text) => text.is_empty(),
        ResponseValue::Number(number) => number.is_empty(),
        ResponseValue::Choice(choice) => choice.is_empty(),
        ResponseValue::YesNo(_) => false,
    }
}

pub struct Spec {
    pub start: NodeId,
    pub nodes: BTreeMap<NodeId, Node>,
}

#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    #[error("spec references unknown node {0:?}")]
    UnknownNode(NodeId),
    #[error(transparent)]
    Transport(#[from] crate::flow::transport::TransportError),
    #[error(transparent)]
    Write(#[from] crate::flow::config_write::WriteError),
}

impl Spec {
    pub async fn walk(
        &self,
        transport: &mut dyn FlowTransport,
        config: &mut Config,
    ) -> Result<Outcome, WalkError> {
        let mut current = self.start.clone();
        loop {
            let node = self
                .nodes
                .get(&current)
                .ok_or_else(|| WalkError::UnknownNode(current.clone()))?;
            let response = transport.ask(&node.prompt).await?;

            if node.optional && response_is_empty(&response) {
                match node.on_success.clone() {
                    Step::Node(next) => {
                        current = next;
                        continue;
                    }
                    Step::Terminal(outcome) => {
                        transport.emit(&outcome).await?;
                        return Ok(outcome);
                    }
                }
            }

            let succeeded = (node.validate)(&response).is_ok();
            if succeeded {
                if let Some((section, key)) = &node.ensure_map_key {
                    config
                        .create_map_key(section, key)
                        .map_err(|reason| WriteError::Config(anyhow::Error::msg(reason)))?;
                }
                if let Some(target) = &node.write_target {
                    write_to_target(config, target, &response)?;
                } else if !node.prop.is_empty() {
                    write_response(config, &node.prop, &response)?;
                }
            }

            match node.resolve(&response) {
                Step::Node(next) => current = next,
                Step::Terminal(outcome) => {
                    transport.emit(&outcome).await?;
                    return Ok(outcome);
                }
            }
        }
    }

    #[must_use]
    pub fn render_tree(&self) -> String {
        let mut rendered = String::new();
        rendered.push_str("```\n");
        let root_label = self
            .nodes
            .get(&self.start)
            .map(Node::display_label)
            .unwrap_or_else(|| self.start.0.clone());
        rendered.push_str(&root_label);
        rendered.push('\n');
        let mut path = vec![self.start.clone()];
        self.render_node(&self.start, "", &mut path, &mut rendered);
        rendered.push_str("```\n");
        rendered
    }

    fn render_node(
        &self,
        node_id: &NodeId,
        prefix: &str,
        path: &mut Vec<NodeId>,
        rendered: &mut String,
    ) {
        let Some(node) = self.nodes.get(node_id) else {
            return;
        };
        self.render_edge(&node.on_success, EdgeKind::Ok, prefix, true, path, rendered);
        self.render_edge(
            &node.on_failure,
            EdgeKind::Err,
            prefix,
            false,
            path,
            rendered,
        );
    }

    fn render_edge(
        &self,
        step: &Step,
        edge: EdgeKind,
        prefix: &str,
        has_sibling_below: bool,
        path: &mut Vec<NodeId>,
        rendered: &mut String,
    ) {
        let connector = if has_sibling_below {
            "├─"
        } else {
            "└─"
        };
        let child_prefix = if has_sibling_below { "│  " } else { "   " };
        match step {
            Step::Terminal(outcome) => {
                let _ = writeln!(
                    rendered,
                    "{prefix}{connector} {} ─> [{}]",
                    edge.as_str(),
                    outcome.label()
                );
            }
            Step::Node(target) => {
                let label = self
                    .nodes
                    .get(target)
                    .map(Node::display_label)
                    .unwrap_or_else(|| target.0.clone());
                if path.contains(target) {
                    let _ = writeln!(
                        rendered,
                        "{prefix}{connector} {} ─> {label} (loop)",
                        edge.as_str()
                    );
                    return;
                }
                let _ = writeln!(rendered, "{prefix}{connector} {} ─> {label}", edge.as_str());
                path.push(target.clone());
                let nested_prefix = format!("{prefix}{child_prefix}");
                self.render_node(target, &nested_prefix, path, rendered);
                path.pop();
            }
        }
    }
}

#[derive(Clone, Copy)]
enum EdgeKind {
    Ok,
    Err,
}

impl EdgeKind {
    fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Ok => "ok",
            EdgeKind::Err => "err",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::transport::{ConfiguredItem, TransportError, TransportResult};
    use crate::response_type::ResponseType;
    use std::collections::VecDeque;
    use tempfile::TempDir;
    use zeroclaw_config::schema::MatrixConfig;

    struct ScriptedTransport {
        scripted: VecDeque<ResponseValue>,
        emitted: Vec<Outcome>,
    }

    impl ScriptedTransport {
        fn new(scripted: Vec<ResponseValue>) -> Self {
            Self {
                scripted: scripted.into(),
                emitted: Vec::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl FlowTransport for ScriptedTransport {
        async fn ask(&mut self, _prompt: &Prompt) -> TransportResult<ResponseValue> {
            self.scripted.pop_front().ok_or(TransportError::Closed)
        }

        async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
            self.emitted.push(outcome.clone());
            Ok(())
        }
    }

    fn test_config() -> (TempDir, Config) {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config
            .channels
            .matrix
            .insert("home".to_string(), MatrixConfig::default());
        (tmp, config)
    }

    fn completed() -> Outcome {
        Outcome::Completed {
            configured: vec![ConfiguredItem {
                layer: "channel".into(),
                instance: "matrix".into(),
            }],
        }
    }

    fn confirm_node() -> Node {
        Node {
            id: NodeId::new("confirm"),
            layer: "agent".into(),
            instance: "scout".into(),
            prop: String::new(),
            optional: false,
            prompt: Prompt::new("Proceed?", ResponseType::YesNo),
            on_success: Step::Node(NodeId::new("token")),
            on_failure: Step::Terminal(Outcome::Cancelled),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(|response| match response {
                ResponseValue::YesNo(true) => Ok(()),
                _ => Err(()),
            }),
        }
    }

    fn token_node() -> Node {
        Node {
            id: NodeId::new("token"),
            layer: "channel".into(),
            instance: "matrix".into(),
            prop: "channels.matrix.access_token".into(),
            optional: false,
            prompt: Prompt::new("Access token", ResponseType::Secret),
            on_success: Step::Terminal(completed()),
            on_failure: Step::Node(NodeId::new("token")),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(|response| match response {
                ResponseValue::Secret(secret) if !secret.expose().is_empty() => Ok(()),
                _ => Err(()),
            }),
        }
    }

    fn spec() -> Spec {
        let mut nodes = BTreeMap::new();
        let confirm = confirm_node();
        let token = token_node();
        nodes.insert(confirm.id.clone(), confirm);
        nodes.insert(token.id.clone(), token);
        Spec {
            start: NodeId::new("confirm"),
            nodes,
        }
    }

    #[tokio::test]
    async fn success_edges_reach_completed_and_persist_secret() {
        use crate::response_type::SecretValue;
        let (_tmp, mut config) = test_config();
        let mut transport = ScriptedTransport::new(vec![
            ResponseValue::YesNo(true),
            ResponseValue::Secret(SecretValue::new("tok".into())),
        ]);
        let outcome = spec().walk(&mut transport, &mut config).await.unwrap();
        assert_eq!(outcome, completed());
        assert_eq!(transport.emitted, vec![outcome]);
        assert_eq!(
            config
                .channels
                .matrix
                .get("home")
                .unwrap()
                .access_token
                .as_deref(),
            Some("tok")
        );
    }

    #[tokio::test]
    async fn failure_on_confirm_cancels() {
        let (_tmp, mut config) = test_config();
        let mut transport = ScriptedTransport::new(vec![ResponseValue::YesNo(false)]);
        let outcome = spec().walk(&mut transport, &mut config).await.unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
    }

    #[tokio::test]
    async fn failure_on_token_loops_back_then_succeeds() {
        use crate::response_type::SecretValue;
        let (_tmp, mut config) = test_config();
        let mut transport = ScriptedTransport::new(vec![
            ResponseValue::YesNo(true),
            ResponseValue::Secret(SecretValue::new(String::new())),
            ResponseValue::Secret(SecretValue::new("tok".into())),
        ]);
        let outcome = spec().walk(&mut transport, &mut config).await.unwrap();
        assert_eq!(outcome, completed());
    }

    #[tokio::test]
    async fn optional_empty_response_skips_without_writing() {
        let (_tmp, mut config) = test_config();
        let mut nodes = BTreeMap::new();
        let optional = Node {
            id: NodeId::new("homeserver"),
            layer: "channel".into(),
            instance: "matrix".into(),
            prop: "channels.matrix.home.homeserver".into(),
            optional: true,
            prompt: Prompt::new("Homeserver", ResponseType::FreeformText),
            on_success: Step::Terminal(completed()),
            on_failure: Step::Node(NodeId::new("homeserver")),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(|_| Ok(())),
        };
        nodes.insert(optional.id.clone(), optional);
        let spec = Spec {
            start: NodeId::new("homeserver"),
            nodes,
        };
        let before = config
            .channels
            .matrix
            .get("home")
            .unwrap()
            .homeserver
            .clone();
        let mut transport =
            ScriptedTransport::new(vec![ResponseValue::FreeformText(String::new())]);
        let outcome = spec.walk(&mut transport, &mut config).await.unwrap();
        assert_eq!(outcome, completed());
        assert_eq!(
            config.channels.matrix.get("home").unwrap().homeserver,
            before,
            "optional skip must not write"
        );
    }

    #[tokio::test]
    async fn optional_non_empty_response_writes() {
        let (_tmp, mut config) = test_config();
        let mut nodes = BTreeMap::new();
        let optional = Node {
            id: NodeId::new("homeserver"),
            layer: "channel".into(),
            instance: "matrix".into(),
            prop: "channels.matrix.home.homeserver".into(),
            optional: true,
            prompt: Prompt::new("Homeserver", ResponseType::FreeformText),
            on_success: Step::Terminal(completed()),
            on_failure: Step::Node(NodeId::new("homeserver")),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: None,
            validate: Box::new(|_| Ok(())),
        };
        nodes.insert(optional.id.clone(), optional);
        let spec = Spec {
            start: NodeId::new("homeserver"),
            nodes,
        };
        let mut transport = ScriptedTransport::new(vec![ResponseValue::FreeformText(
            "https://example.org".into(),
        )]);
        spec.walk(&mut transport, &mut config).await.unwrap();
        assert_eq!(
            config.channels.matrix.get("home").unwrap().homeserver,
            "https://example.org"
        );
    }

    #[tokio::test]
    async fn write_target_node_writes_personality_file_to_workspace() {
        use crate::flow::config_write::WriteTarget;
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let mut nodes = BTreeMap::new();
        let author = Node {
            id: NodeId::new("soul"),
            layer: "agent".into(),
            instance: "scout".into(),
            prop: String::new(),
            optional: false,
            prompt: Prompt::new("Author SOUL.md", ResponseType::FreeformText),
            on_success: Step::Terminal(completed()),
            on_failure: Step::Node(NodeId::new("soul")),
            branches: Vec::new(),
            ensure_map_key: None,
            write_target: Some(WriteTarget::WorkspaceFileFromResponse {
                agent_alias: "scout".into(),
                filename: "SOUL.md".into(),
            }),
            validate: Box::new(|response| match response {
                ResponseValue::FreeformText(text) if !text.is_empty() => Ok(()),
                _ => Err(()),
            }),
        };
        nodes.insert(author.id.clone(), author);
        let spec = Spec {
            start: NodeId::new("soul"),
            nodes,
        };
        let mut transport =
            ScriptedTransport::new(vec![ResponseValue::FreeformText("this is my soul".into())]);
        let outcome = spec.walk(&mut transport, &mut config).await.unwrap();
        assert_eq!(outcome, completed());
        let written =
            std::fs::read_to_string(config.agent_workspace_dir("scout").join("SOUL.md")).unwrap();
        assert_eq!(written, "this is my soul");
    }

    #[test]
    fn renders_markdown_ascii_tree_with_layered_identity() {
        let rendered = spec().render_tree();
        let expected = "\
```
agent:scout ask (YesNo)
├─ ok ─> channel:matrix ask (Secret)
│  ├─ ok ─> [completed: channel:matrix]
│  └─ err ─> channel:matrix ask (Secret) (loop)
└─ err ─> [cancelled]
```
";
        assert_eq!(rendered, expected);
    }
}
