use serde_json::{Value, json};
use zeroclaw_tool_call_parser::parse_tool_calls;

const URL: &str = "https://example.com/it's;still=one?value=$(echo_test)&quoted=`value`";
const QUERY: &str = "Rust's async; $(echo_test) & browser | API";

fn assert_call(response: &str, name: &str, arguments: &Value) {
    let (_, calls) = parse_tool_calls(response);
    assert_eq!(calls.len(), 1, "{response}");
    assert_eq!(calls[0].name, name, "{response}");
    assert_eq!(&calls[0].arguments, arguments, "{response}");
}

#[test]
fn glm_browser_and_search_values_keep_their_tool_semantics() {
    for (raw, expected, parameter, value) in [
        ("browser_open", "browser_open", "url", URL),
        ("browser", "browser", "url", URL),
        ("web_search", "web_search_tool", "query", QUERY),
        ("web_search_tool", "web_search_tool", "query", QUERY),
    ] {
        let arguments = json!({ parameter: value });
        for response in [
            format!("{raw}/{parameter}>{value}"),
            format!("<tool_call>{raw}>{value}</tool_call>"),
            format!("<tool_call>{raw}>{value}</invoke>"),
            format!("<tool_call>{raw}>{value}"),
        ] {
            assert_call(&response, expected, &arguments);
        }
    }
}

#[test]
fn json_formats_preserve_browser_and_search_argument_objects() {
    for (raw, expected, arguments) in [
        ("browser_open", "browser_open", json!({ "url": URL })),
        (
            "browser",
            "browser",
            json!({ "action": "snapshot", "interactive_only": true, "depth": 3 }),
        ),
        ("web_search", "web_search_tool", json!({ "query": QUERY })),
        (
            "web_search_tool",
            "web_search_tool",
            json!({ "query": QUERY }),
        ),
    ] {
        let call = json!({ "name": raw, "arguments": arguments });
        let nested = json!({
            "tool_calls": [{ "function": { "name": raw, "arguments": arguments.to_string() } }]
        });
        for response in [
            call.to_string(),
            nested.to_string(),
            json!([call]).to_string(),
            format!("<tool_call>{call}</tool_call>"),
            format!("<tool_call><{raw}>{arguments}</{raw}></tool_call>"),
            format!("<invoke name=\"{raw}\">{arguments}</invoke>"),
            format!("```tool {raw}\n{arguments}\n```"),
            format!("{raw}/{arguments}"),
        ] {
            assert_call(&response, expected, &arguments);
        }
    }
}

#[test]
fn textual_argument_formats_preserve_browser_actions_and_search_queries() {
    for (raw, expected, arguments) in [
        ("browser_open", "browser_open", json!({ "url": URL })),
        (
            "browser",
            "browser",
            json!({ "action": "open", "url": URL }),
        ),
        ("web_search", "web_search_tool", json!({ "query": QUERY })),
    ] {
        let pairs: Vec<_> = arguments
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key, value.as_str().unwrap()))
            .collect();
        let xml: String = pairs
            .iter()
            .map(|(key, value)| format!("<{key}>{value}</{key}>"))
            .collect();
        let minimax: String = pairs
            .iter()
            .map(|(key, value)| format!("<parameter name=\"{key}\">{value}</parameter>"))
            .collect();
        let attributes = pairs
            .iter()
            .map(|(key, value)| format!("{key}=\"{value}\""))
            .collect::<Vec<_>>();
        let function: String = pairs
            .iter()
            .map(|(key, value)| format!("{key}>{value}\n"))
            .collect();
        let perl = pairs
            .iter()
            .map(|(key, value)| format!("--{key} \"{value}\""))
            .collect::<Vec<_>>()
            .join(" ");
        for response in [
            format!("<tool_call><{raw}>{xml}</{raw}></tool_call>"),
            format!(
                "<minimax:tool_call><invoke name=\"{raw}\">{minimax}</invoke></minimax:tool_call>"
            ),
            format!("<tool_call>{raw} {}</tool_call>", attributes.join(" ")),
            format!("<tool_call>{raw}({})</tool_call>", attributes.join(", ")),
            format!("<FunctionCall>{raw}<code>{function}</code></FunctionCall>"),
            format!("TOOL_CALL {{ tool => \"{raw}\", args => {{ {perl} }}}} /TOOL_CALL"),
        ] {
            assert_call(&response, expected, &arguments);
        }
        if pairs.len() > 1 {
            let yaml: String = pairs
                .iter()
                .map(|(key, value)| format!("{key}: {value}\n"))
                .collect();
            assert_call(
                &format!("<tool_call>{raw}>\n{yaml}</tool_call>"),
                expected,
                &arguments,
            );
        }
    }
}

#[test]
fn explicit_shell_commands_still_parse_as_shell() {
    let command = "printf 'browser_open and web_search'";
    let arguments = json!({ "command": command });
    for raw in ["shell", "bash", "sh", "exec", "command", "cmd"] {
        for response in [
            format!("{raw}/command>{command}"),
            format!("<tool_call>{raw}>{command}</tool_call>"),
            json!({ "name": raw, "arguments": arguments }).to_string(),
        ] {
            assert_call(&response, "shell", &arguments);
        }
    }
}
