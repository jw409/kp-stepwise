//! Integration tests for kp-stepwise.
//! No external API calls needed - tests the MCP server via JSON-RPC over stdin/stdout.

use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

struct McpClient {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u32,
}

impl McpClient {
    async fn new() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kp-stepwise"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("DISABLE_STEP_LOGGING", "true")
            .spawn()
            .expect("Failed to start server");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);

        let mut client = Self {
            child,
            stdin,
            reader,
            next_id: 1,
        };

        // Initialize
        let _ = client
            .call(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0.1"}
                }),
            )
            .await;

        // Send initialized notification
        client.send_notification("notifications/initialized").await;

        client
    }

    async fn send_notification(&mut self, method: &str) {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.send_call(method, params).await;
        self.read_response().await
    }

    async fn send_call(&mut self, method: &str, params: Value) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
        id
    }

    async fn read_response(&mut self) -> Value {
        let mut buf = String::new();
        self.reader.read_line(&mut buf).await.unwrap();
        serde_json::from_str(&buf).unwrap()
    }

    async fn tool_call(&mut self, name: &str, mut args: Value) -> Value {
        if name == "stepwise_plan" {
            args.as_object_mut()
                .expect("tool arguments should be an object")
                .entry("channelId")
                .or_insert_with(|| json!("test-main"));
        }
        self.call("tools/call", json!({"name": name, "arguments": args}))
            .await
    }

    fn get_text(resp: &Value) -> &str {
        resp["result"]["content"][0]["text"].as_str().unwrap_or("")
    }

    fn get_parsed(resp: &Value) -> Value {
        let text = Self::get_text(resp);
        serde_json::from_str(text).unwrap_or(json!({"_parse_error": text}))
    }

    fn is_error(resp: &Value) -> bool {
        resp.get("error").is_some()
    }

    fn is_tool_error(resp: &Value) -> bool {
        resp["result"]["isError"].as_bool().unwrap_or(false)
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[tokio::test]
async fn test_initialize() {
    let mut client = McpClient::new().await;
    let resp = client.call("tools/list", json!({})).await;
    // Server should respond without error
    assert!(!McpClient::is_error(&resp));
}

#[tokio::test]
async fn test_tools_list() {
    let mut client = McpClient::new().await;
    let resp = client.call("tools/list", json!({})).await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "Should have exactly 1 tool");

    let tool = &tools[0];
    assert_eq!(tool["name"].as_str().unwrap(), "stepwise_plan");
    // Should have inputSchema
    assert!(tool["inputSchema"].is_object(), "Should have inputSchema");
    assert!(tool["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "channelId"));
}

#[tokio::test]
async fn test_legacy_param_names_accepted() {
    // Pre-rename callers used thought/thoughtNumber/totalThoughts; serde
    // aliases must keep accepting them even though the schema advertises
    // the new names.
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "thought": "Legacy caller payload",
                "thoughtNumber": 1,
                "totalThoughts": 3,
                "nextThoughtNeeded": true
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(
        !McpClient::is_tool_error(&resp),
        "legacy param names should deserialize via aliases"
    );

    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(parsed["stepNumber"], 1);
    assert_eq!(parsed["totalSteps"], 3);
}

#[tokio::test]
async fn test_basic_step() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Repository evidence narrows the problem to one boundary",
                "roomId": "integration-room",
                "stepNumber": 1,
                "totalSteps": 3,
                "turnId": "turn-1",
                "checkpointKind": "observation",
                "evidence": ["src/server.rs:159"],
                "openQuestions": ["Does the client pipeline calls?"],
                "nextAction": "Probe ordering"
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(parsed["stepNumber"], 1);
    assert_eq!(parsed["totalSteps"], 3);
    assert_eq!(parsed["nextStepNeeded"], true);
    assert_eq!(parsed["stepCount"], 1);
    assert_eq!(parsed["expectedNextStep"], 2);
    assert_eq!(parsed["turnId"], "turn-1");
    assert_eq!(parsed["checkpointKind"], "observation");
    assert!(parsed["sessionId"].is_string());
    assert_eq!(parsed["roomId"], "integration-room");
    assert_eq!(parsed["channelId"], "test-main");
    assert!(parsed["logMode"].is_string());
}

#[tokio::test]
async fn test_out_of_order_step_rejected_then_recoverable() {
    let mut client = McpClient::new().await;
    let first = client
        .tool_call(
            "stepwise_plan",
            json!({"step": "First checkpoint", "stepNumber": 1, "totalSteps": 3}),
        )
        .await;
    assert!(!McpClient::is_tool_error(&first));

    let early = client
        .tool_call(
            "stepwise_plan",
            json!({"step": "Arrived too early", "stepNumber": 3, "totalSteps": 3}),
        )
        .await;
    assert!(McpClient::is_tool_error(&early));
    let error = McpClient::get_parsed(&early);
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("expected 2, got 3"));

    let second = client
        .tool_call(
            "stepwise_plan",
            json!({"step": "Second checkpoint", "stepNumber": 2, "totalSteps": 3}),
        )
        .await;
    assert!(!McpClient::is_tool_error(&second));
    let parsed = McpClient::get_parsed(&second);
    assert_eq!(parsed["stepCount"], 2);
    assert_eq!(parsed["expectedNextStep"], 3);
}

#[tokio::test]
async fn test_parallel_channels_start_at_one_without_state_pollution() {
    let mut client = McpClient::new().await;

    let id_a = client
        .send_call(
            "tools/call",
            json!({
                "name": "stepwise_plan",
                "arguments": {
                "roomId": "eval-repair",
                "channelId": "opus-gold-type",
                "step": "Agent A first checkpoint",
                "stepNumber": 1,
                "totalSteps": 3,
                "confidence": 0.2
                }
            }),
        )
        .await;
    let id_b = client
        .send_call(
            "tools/call",
            json!({
                "name": "stepwise_plan",
                "arguments": {
                "roomId": "eval-repair",
                "channelId": "sonnet-corpus-replay",
                "step": "Agent B first checkpoint",
                "stepNumber": 1,
                "totalSteps": 2,
                "confidence": 0.9
                }
            }),
        )
        .await;

    let first = client.read_response().await;
    let second = client.read_response().await;
    let (agent_a, agent_b) = if first["id"] == id_a {
        (first, second)
    } else {
        assert_eq!(first["id"], id_b);
        (second, first)
    };
    assert_eq!(agent_a["id"], id_a);
    assert_eq!(agent_b["id"], id_b);
    assert!(!McpClient::is_tool_error(&agent_a));
    assert!(!McpClient::is_tool_error(&agent_b));
    let parsed_a = McpClient::get_parsed(&agent_a);
    let parsed_b = McpClient::get_parsed(&agent_b);
    assert_eq!(parsed_a["stepCount"], 1);
    assert_eq!(parsed_b["stepCount"], 1);
    assert_eq!(parsed_a["expectedNextStep"], 2);
    assert_eq!(parsed_b["expectedNextStep"], 2);
    assert_eq!(parsed_a["usageStats"]["lowConfWithoutBranchCount"], 1);
    assert_eq!(parsed_b["usageStats"]["lowConfWithoutBranchCount"], 0);

    let channels = client
        .call(
            "resources/read",
            json!({"uri": "stepwise://rooms/eval-repair/channels"}),
        )
        .await;
    assert!(!McpClient::is_error(&channels));
    let text = channels["result"]["contents"][0]["text"]
        .as_str()
        .expect("room channel index");
    let index: Value = serde_json::from_str(text).unwrap();
    assert_eq!(index["channelCount"], 2);
    let channel_ids: Vec<&str> = index["channels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|channel| channel["channelId"].as_str())
        .collect();
    assert_eq!(channel_ids, vec!["opus-gold-type", "sonnet-corpus-replay"]);
}

#[tokio::test]
async fn test_session_resource_is_inspectable() {
    let mut client = McpClient::new().await;
    let step = client
        .tool_call(
            "stepwise_plan",
            json!({
                "roomId": "inspect-room",
                "channelId": "inspect-agent",
                "step": "Inspectable checkpoint",
                "stepNumber": 1,
                "totalSteps": 2
            }),
        )
        .await;
    assert!(!McpClient::is_tool_error(&step));

    let response = client
        .call(
            "resources/read",
            json!({"uri": "stepwise://rooms/inspect-room/channels/inspect-agent/session"}),
        )
        .await;
    assert!(!McpClient::is_error(&response));
    let text = response["result"]["contents"][0]["text"]
        .as_str()
        .expect("session resource text");
    let session: Value = serde_json::from_str(text).unwrap();
    assert!(session["sessionId"].is_string());
    assert!(session["profile"].is_string());
    assert!(session["logMode"].is_string());
    assert_eq!(session["roomId"], "inspect-room");
    assert_eq!(session["channelId"], "inspect-agent");
    assert_eq!(session["checkpointCount"], 1);
    assert_eq!(session["expectedNextStep"], 2);
}

#[tokio::test]
async fn test_step_chain() {
    let mut client = McpClient::new().await;

    // Step 1
    let r1 = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "First: understand the problem",
                "stepNumber": 1,
                "totalSteps": 3
            }),
        )
        .await;
    let p1 = McpClient::get_parsed(&r1);
    assert_eq!(p1["stepCount"], 1);

    // Step 2
    let r2 = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Second: design the solution",
                "stepNumber": 2,
                "totalSteps": 3
            }),
        )
        .await;
    let p2 = McpClient::get_parsed(&r2);
    assert_eq!(p2["stepCount"], 2);

    // Step 3
    let r3 = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Third: implement",
                "stepNumber": 3,
                "totalSteps": 3,
                "nextStepNeeded": false
            }),
        )
        .await;
    let p3 = McpClient::get_parsed(&r3);
    assert_eq!(p3["stepCount"], 3);
    assert_eq!(p3["nextStepNeeded"], false);
}

#[tokio::test]
async fn test_branching() {
    let mut client = McpClient::new().await;

    // Linear step first
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Base analysis",
                "stepNumber": 1,
                "totalSteps": 4
            }),
        )
        .await;

    // Branch from step 1
    let r2 = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Alternative approach A",
                "stepNumber": 2,
                "totalSteps": 4,
                "branchFromStep": 1,
                "branchId": "approach-a"
            }),
        )
        .await;
    let p2 = McpClient::get_parsed(&r2);
    let branches = p2["branches"].as_array().unwrap();
    assert!(
        branches.iter().any(|b| b.as_str() == Some("approach-a")),
        "Should track branch 'approach-a', got: {:?}",
        branches
    );

    // Another branch
    let r3 = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Alternative approach B",
                "stepNumber": 3,
                "totalSteps": 4,
                "branchFromStep": 1,
                "branchId": "approach-b"
            }),
        )
        .await;
    let p3 = McpClient::get_parsed(&r3);
    let branches = p3["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2, "Should have 2 branches");
}

#[tokio::test]
async fn test_confidence_low_guidance() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "I'm uncertain about the approach",
                "stepNumber": 1,
                "totalSteps": 3,
                "confidence": 0.3
            }),
        )
        .await;
    let parsed = McpClient::get_parsed(&resp);
    let guidance = parsed["guidance"].as_str().unwrap_or("");
    assert!(
        guidance.to_lowercase().contains("branch"),
        "Low confidence should suggest branching, got: {}",
        guidance
    );
}

#[tokio::test]
async fn test_high_confidence_exit() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "The answer is clear now",
                "stepNumber": 2,
                "totalSteps": 5,
                "confidence": 0.85
            }),
        )
        .await;
    let parsed = McpClient::get_parsed(&resp);
    let guidance = parsed["guidance"].as_str().unwrap_or("");
    assert!(
        guidance.to_lowercase().contains("done")
            || guidance.to_lowercase().contains("sufficient")
            || guidance.to_lowercase().contains("exit")
            || guidance.to_lowercase().contains("confidence"),
        "High confidence should suggest early exit, got: {}",
        guidance
    );
}

#[tokio::test]
async fn test_first_call_guidance() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Starting analysis",
                "stepNumber": 1,
                "totalSteps": 5
            }),
        )
        .await;
    let parsed = McpClient::get_parsed(&resp);
    assert!(
        parsed.get("firstCallGuidance").is_some(),
        "First step should include firstCallGuidance"
    );
    let guidance = parsed["firstCallGuidance"].as_str().unwrap();
    assert!(
        guidance.contains("DECIDE(confidence)"),
        "First call guidance should contain decision tree"
    );

    // Second step should NOT have firstCallGuidance
    let resp2 = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Continuing",
                "stepNumber": 2,
                "totalSteps": 5
            }),
        )
        .await;
    let parsed2 = McpClient::get_parsed(&resp2);
    assert!(
        parsed2.get("firstCallGuidance").is_none(),
        "Second step should not have firstCallGuidance"
    );
}

#[tokio::test]
async fn test_usage_tracking() {
    let mut client = McpClient::new().await;

    // Send 5 linear steps without branching
    for i in 1..=5 {
        let resp = client
            .tool_call(
                "stepwise_plan",
                json!({
                    "step": format!("Linear step {}", i),
                    "stepNumber": i,
                    "totalSteps": 8
                }),
            )
            .await;

        if i >= 4 {
            let parsed = McpClient::get_parsed(&resp);
            let usage_stats = &parsed["usageStats"];
            assert!(
                usage_stats["needsBranching"].as_bool().unwrap_or(false),
                "After {} linear steps, needsBranching should be true",
                i
            );
            assert!(
                usage_stats["consecutiveLinearSteps"].as_u64().unwrap() >= 4,
                "Should track consecutive linear steps"
            );
        }
    }
}

#[tokio::test]
async fn test_explore_mode() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Exploring alternatives",
                "stepNumber": 1,
                "totalSteps": 5,
                "continuationMode": "explore",
                "exploreCount": 3,
                "proposals": ["Use recursion", "Use iteration", "Use memoization"]
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(parsed["nextStepNeeded"], true);
    assert_eq!(parsed["stepCount"], 1);
}

#[tokio::test]
async fn test_done_mode() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "The answer is 42",
                "stepNumber": 2,
                "totalSteps": 5,
                "continuationMode": "done",
                "doneReason": "sufficient",
                "confidence": 0.9
            }),
        )
        .await;
    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(
        parsed["nextStepNeeded"], false,
        "done mode should set nextStepNeeded=false"
    );
}

#[tokio::test]
async fn test_validation_empty_step() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "",
                "stepNumber": 1,
                "totalSteps": 3
            }),
        )
        .await;
    // Should return tool error (isError=true) not protocol error
    assert!(
        McpClient::is_tool_error(&resp),
        "Empty step should return tool error"
    );
    let parsed = McpClient::get_parsed(&resp);
    assert!(
        parsed["error"].as_str().unwrap_or("").contains("non-empty"),
        "Error should mention non-empty requirement"
    );
}

#[tokio::test]
async fn test_validation_zero_step_number() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Test",
                "stepNumber": 0,
                "totalSteps": 3
            }),
        )
        .await;
    assert!(
        McpClient::is_tool_error(&resp),
        "Zero stepNumber should return tool error"
    );
}

#[tokio::test]
async fn test_auto_adjust_total_steps() {
    let mut client = McpClient::new().await;
    // stepNumber > totalSteps should auto-adjust
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Went over estimate",
                "stepNumber": 5,
                "totalSteps": 3
            }),
        )
        .await;
    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(
        parsed["totalSteps"], 5,
        "totalSteps should be adjusted to match stepNumber"
    );
}

#[tokio::test]
async fn test_search_query_passthrough() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Need more info about X",
                "stepNumber": 1,
                "totalSteps": 3,
                "searchQuery": "how does X work"
            }),
        )
        .await;
    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(
        parsed["pendingSearchQuery"], "how does X work",
        "Should pass through search query"
    );
    assert!(
        parsed.get("hint").is_some(),
        "Should include hint about executing search"
    );
}

#[tokio::test]
async fn test_revision() {
    let mut client = McpClient::new().await;

    // Initial step
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Initial analysis",
                "stepNumber": 1,
                "totalSteps": 3
            }),
        )
        .await;

    // Revision of step 1
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Actually, my initial analysis was wrong because...",
                "stepNumber": 2,
                "totalSteps": 3,
                "isRevision": true,
                "revisesStep": 1
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(parsed["stepCount"], 2);
}

#[tokio::test]
async fn test_layer_abstraction() {
    let mut client = McpClient::new().await;
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Understanding the problem domain",
                "stepNumber": 1,
                "totalSteps": 3,
                "layer": 1
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    // Layer should be accepted without error
    let parsed = McpClient::get_parsed(&resp);
    assert_eq!(parsed["stepNumber"], 1);
}

#[tokio::test]
async fn test_confidence_clamping() {
    let mut client = McpClient::new().await;

    // Confidence > 1.0 should be clamped
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Very confident",
                "stepNumber": 1,
                "totalSteps": 2,
                "confidence": 1.5
            }),
        )
        .await;
    assert!(!McpClient::is_tool_error(&resp));
    // Should not crash - response should be valid
    let parsed = McpClient::get_parsed(&resp);
    assert!(
        parsed.get("guidance").is_some(),
        "High confidence should produce guidance"
    );
}

#[tokio::test]
async fn test_merge_mode() {
    let mut client = McpClient::new().await;

    // Create two branches first
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Base",
                "stepNumber": 1,
                "totalSteps": 4
            }),
        )
        .await;
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Branch A",
                "stepNumber": 2,
                "totalSteps": 4,
                "branchFromStep": 1,
                "branchId": "merge-a"
            }),
        )
        .await;
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Branch B",
                "stepNumber": 3,
                "totalSteps": 4,
                "branchFromStep": 1,
                "branchId": "merge-b"
            }),
        )
        .await;

    // Merge
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Combining insights from both branches",
                "stepNumber": 4,
                "totalSteps": 4,
                "continuationMode": "merge"
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    let branches = parsed["branches"].as_array().unwrap();
    assert_eq!(branches.len(), 2, "Should still have both branches tracked");
}

#[tokio::test]
async fn test_subagent_spawn_hint_on_parallel_branch() {
    let mut client = McpClient::new().await;

    // Base step
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Analyzing the problem",
                "stepNumber": 1,
                "totalSteps": 5
            }),
        )
        .await;

    // Branch with parallel strategy and 3+ proposals → should trigger subagent hint
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Three approaches to explore independently",
                "stepNumber": 2,
                "totalSteps": 5,
                "branchFromStep": 1,
                "branchId": "approach-a",
                "branchStrategy": "parallel",
                "proposals": [
                    "Approach A: use caching layer",
                    "Approach B: optimize queries",
                    "Approach C: add read replicas"
                ],
                "confidence": 0.4
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    let hints = parsed["hints"].as_array().expect("should have hints");
    let subagent_hint = hints
        .iter()
        .find(|h| h["kind"] == "subagent_spawn_available");
    assert!(
        subagent_hint.is_some(),
        "Should emit subagent_spawn_available hint for parallel branch with 3+ proposals"
    );
    let msg = subagent_hint.unwrap()["message"].as_str().unwrap();
    assert!(
        msg.contains("approach-a"),
        "Hint should reference branch name"
    );
    assert!(
        msg.contains("3 proposals"),
        "Hint should mention proposal count"
    );
}

#[tokio::test]
async fn test_subagent_orchestration_hint_on_three_branches() {
    let mut client = McpClient::new().await;

    // Base step
    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Analyzing the problem",
                "stepNumber": 1,
                "totalSteps": 8
            }),
        )
        .await;

    // Create 3 branches
    for (i, name) in ["branch-x", "branch-y", "branch-z"].iter().enumerate() {
        let _ = client
            .tool_call(
                "stepwise_plan",
                json!({
                    "step": format!("Exploring {}", name),
                    "stepNumber": (i + 2) as u32,
                    "totalSteps": 8,
                    "branchFromStep": 1,
                    "branchId": name
                }),
            )
            .await;
    }

    // Non-merge step should trigger subagent_orchestration hint
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Continuing analysis without merging",
                "stepNumber": 5,
                "totalSteps": 8,
                "continuationMode": "continue"
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));
    assert!(!McpClient::is_tool_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    let hints = parsed["hints"].as_array().expect("should have hints");
    let orch_hint = hints.iter().find(|h| h["kind"] == "subagent_orchestration");
    assert!(
        orch_hint.is_some(),
        "Should emit subagent_orchestration hint when 3+ branches exist"
    );
    let msg = orch_hint.unwrap()["message"].as_str().unwrap();
    assert!(msg.contains("3 branches"), "Should mention branch count");
    // All three branch names should appear
    assert!(msg.contains("branch-x"), "Should list branch-x");
    assert!(msg.contains("branch-y"), "Should list branch-y");
    assert!(msg.contains("branch-z"), "Should list branch-z");
}

#[tokio::test]
async fn test_subagent_orchestration_suppressed_during_merge() {
    let mut client = McpClient::new().await;

    let _ = client
        .tool_call(
            "stepwise_plan",
            json!({"step": "Base", "stepNumber": 1, "totalSteps": 6}),
        )
        .await;

    // Create 3 branches
    for (i, name) in ["m-a", "m-b", "m-c"].iter().enumerate() {
        let _ = client
            .tool_call(
                "stepwise_plan",
                json!({
                    "step": format!("Branch {}", name),
                    "stepNumber": (i + 2) as u32,
                    "totalSteps": 6,
                    "branchFromStep": 1,
                    "branchId": name
                }),
            )
            .await;
    }

    // Merge step should NOT trigger subagent_orchestration
    let resp = client
        .tool_call(
            "stepwise_plan",
            json!({
                "step": "Merging all branches",
                "stepNumber": 5,
                "totalSteps": 6,
                "continuationMode": "merge",
                "mergeBranches": ["m-a", "m-b", "m-c"]
            }),
        )
        .await;
    assert!(!McpClient::is_error(&resp));

    let parsed = McpClient::get_parsed(&resp);
    let empty = vec![];
    let hints = parsed["hints"].as_array().unwrap_or(&empty);
    let orch_hint = hints.iter().find(|h| h["kind"] == "subagent_orchestration");
    assert!(
        orch_hint.is_none(),
        "Should NOT emit subagent_orchestration during merge"
    );
}
