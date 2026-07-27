use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, ListResourceTemplatesResult, ReadResourceRequestParams,
    ReadResourceResult, ResourceContents, ServerInfo,
};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::logging::PersistentLogger;
use crate::planner::{PlanEngine, StepData};
use crate::profiles;

// ============================================================================
// MCP Parameter struct — maps to the tool's JSON Schema
// ============================================================================

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct StepwisePlanParams {
    /// Independent planner lane within a room. Every parallel agent must use a unique channel ID.
    pub channel_id: String,

    /// Shared orchestration scope. Defaults to the MCP host session when omitted.
    #[serde(default)]
    pub room_id: Option<String>,

    /// Concise, externally reviewable checkpoint: conclusion, evidence update, decision, or next action
    #[serde(alias = "thought")]
    pub step: String,

    /// Current step number
    #[schemars(range(min = 1))]
    #[serde(alias = "thoughtNumber")]
    pub step_number: u32,

    /// Estimated total steps needed
    #[schemars(range(min = 1))]
    #[serde(alias = "totalThoughts")]
    pub total_steps: u32,

    /// Stable caller-defined turn ID for grouping checkpoints across a multi-turn workflow
    #[serde(default)]
    pub turn_id: Option<String>,

    /// Checkpoint type: observation, hypothesis, decision, action, result, revision, or handoff
    #[serde(default)]
    pub checkpoint_kind: Option<String>,

    /// Short evidence references supporting this checkpoint (files, tests, tool results, or facts)
    #[serde(default)]
    pub evidence: Option<Vec<String>>,

    /// Unresolved questions that should survive into later turns
    #[serde(default)]
    pub open_questions: Option<Vec<String>>,

    /// Concrete next action, if one is known
    #[serde(default)]
    pub next_action: Option<String>,

    /// Whether another step is needed (backwards compat, prefer continuation_mode)
    #[serde(default, alias = "nextThoughtNeeded")]
    pub next_step_needed: Option<bool>,

    /// Whether this revises an earlier step
    #[serde(default)]
    pub is_revision: Option<bool>,

    /// Which step is being reconsidered
    #[serde(default, alias = "revisesThought")]
    #[schemars(range(min = 1))]
    pub revises_step: Option<u32>,

    /// PRIMARY: Branching point step number - USE LIBERALLY
    #[serde(default, alias = "branchFromThought")]
    #[schemars(range(min = 1))]
    pub branch_from_step: Option<u32>,

    /// PRIMARY: Branch identifier - descriptive name for this exploration path
    #[serde(default)]
    pub branch_id: Option<String>,

    /// If more steps are needed
    #[serde(default, alias = "needsMoreThoughts")]
    pub needs_more_steps: Option<bool>,

    /// How to continue: explore (generate alternatives), done (early exit), delegate (pass to next layer), branch (alternative path), merge (combine branches), continue (default linear)
    #[serde(default)]
    pub continuation_mode: Option<String>,

    /// Number of parallel alternatives to generate (default: 4, max: 5)
    #[serde(default)]
    #[schemars(range(min = 1, max = 5))]
    pub explore_count: Option<u32>,

    /// Lightweight descriptions of alternatives being considered
    #[serde(default)]
    pub proposals: Option<Vec<String>>,

    /// Abstraction layer: 1=problem, 2=approach, 3=details
    #[serde(default)]
    #[schemars(range(min = 1, max = 5))]
    pub layer: Option<u32>,

    /// Pass proposals to smarter agent/layer for selection
    #[serde(default)]
    pub delegate_to_next_layer: Option<bool>,

    /// How to handle branches: parallel (explore all), sequential (one at a time), converge (merge results)
    #[serde(default)]
    pub branch_strategy: Option<String>,

    /// Merge insights from these branch IDs into this step (continuation_mode should be "merge")
    #[serde(default)]
    pub merge_branches: Option<Vec<String>>,

    /// Confidence in current answer (0.0-1.0). Below 0.6: consider branching. Above 0.75: consider early exit.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub confidence: Option<f64>,

    /// Why stopping: complete (fully solved), sufficient (good enough), blocked (can't proceed), delegate (passing up)
    #[serde(default)]
    pub done_reason: Option<String>,

    /// Token efficiency: compact (last 2 steps), normal (last 5), expanded (all)
    #[serde(default)]
    pub context_window: Option<String>,

    /// What to search for before next step (orchestrator will execute)
    #[serde(default)]
    pub search_query: Option<String>,

    /// Previous search results (provided by orchestrator)
    #[serde(default)]
    pub search_context: Option<String>,

    /// Should orchestrator search before next step?
    #[serde(default)]
    pub incorporate_search: Option<bool>,
}

impl From<StepwisePlanParams> for StepData {
    fn from(p: StepwisePlanParams) -> Self {
        StepData {
            step: p.step,
            step_number: p.step_number,
            total_steps: p.total_steps,
            next_step_needed: p.next_step_needed.unwrap_or(true),
            turn_id: p.turn_id,
            checkpoint_kind: p.checkpoint_kind,
            evidence: p.evidence,
            open_questions: p.open_questions,
            next_action: p.next_action,
            is_revision: p.is_revision,
            revises_step: p.revises_step,
            branch_from_step: p.branch_from_step,
            branch_id: p.branch_id,
            needs_more_steps: p.needs_more_steps,
            continuation_mode: p.continuation_mode,
            explore_count: p.explore_count,
            proposals: p.proposals,
            layer: p.layer,
            delegate_to_next_layer: p.delegate_to_next_layer,
            branch_strategy: p.branch_strategy,
            merge_branches: p.merge_branches,
            confidence: p.confidence,
            done_reason: p.done_reason,
            context_window: p.context_window,
            search_query: p.search_query,
            search_context: p.search_context,
            incorporate_search: p.incorporate_search,
        }
    }
}

// ============================================================================
// MCP Server
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChannelRoute {
    room_id: String,
    channel_id: String,
}

type SharedEngine = Arc<Mutex<PlanEngine>>;

pub struct StepwiseServer {
    engines: Mutex<HashMap<ChannelRoute, SharedEngine>>,
    profile: profiles::TuningProfile,
    model_id: String,
    client_type: String,
    session_id: String,
    default_room_id: String,
    tool_router: ToolRouter<Self>,
}

impl StepwiseServer {
    fn new(
        profile: profiles::TuningProfile,
        model_id: String,
        client_type: String,
        session_id: String,
    ) -> Self {
        Self {
            engines: Mutex::new(HashMap::new()),
            profile,
            model_id,
            client_type,
            default_room_id: session_id.clone(),
            session_id,
            tool_router: StepwiseServer::tool_router(),
        }
    }

    fn engine_for(&self, route: &ChannelRoute) -> Result<SharedEngine, McpError> {
        let mut engines = self.engines.lock().map_err(|e| {
            McpError::internal_error(format!("engine registry lock poisoned: {e}"), None)
        })?;

        Ok(engines
            .entry(route.clone())
            .or_insert_with(|| {
                Arc::new(Mutex::new(PlanEngine::new_scoped(
                    self.profile.clone(),
                    self.model_id.clone(),
                    self.client_type.clone(),
                    &self.session_id,
                    &route.room_id,
                    &route.channel_id,
                )))
            })
            .clone())
    }

    fn existing_engine(&self, route: &ChannelRoute) -> Result<Option<SharedEngine>, McpError> {
        let engines = self.engines.lock().map_err(|e| {
            McpError::internal_error(format!("engine registry lock poisoned: {e}"), None)
        })?;
        Ok(engines.get(route).cloned())
    }

    fn validate_route_id(kind: &str, value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err(format!("{kind} must be non-empty"));
        }
        if value.len() > 128 {
            return Err(format!("{kind} must be at most 128 bytes"));
        }
        if matches!(value, "." | "..") {
            return Err(format!("{kind} cannot be '.' or '..'"));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!(
                "{kind} may contain only ASCII letters, digits, '-', '_', and '.'"
            ));
        }
        Ok(())
    }

    fn tool_error(message: impl Into<String>) -> CallToolResult {
        let err_json = serde_json::json!({
            "error": message.into(),
            "status": "failed"
        });
        let text = serde_json::to_string_pretty(&err_json).unwrap_or_default();
        CallToolResult::error(vec![Content::text(text)])
    }
}

#[rmcp::tool_router]
impl StepwiseServer {
    /// Maintain a concise, inspectable decision ledger across turns. Record only externally reviewable conclusions, evidence, alternatives, and next actions.
    #[rmcp::tool(name = "stepwise_plan")]
    fn stepwise_plan(
        &self,
        Parameters(params): Parameters<StepwisePlanParams>,
    ) -> Result<CallToolResult, McpError> {
        let route = ChannelRoute {
            room_id: params
                .room_id
                .clone()
                .unwrap_or_else(|| self.default_room_id.clone()),
            channel_id: params.channel_id.clone(),
        };

        if let Err(message) = Self::validate_route_id("roomId", &route.room_id)
            .and_then(|_| Self::validate_route_id("channelId", &route.channel_id))
        {
            return Ok(Self::tool_error(message));
        }

        let data: StepData = params.into();
        let engine = self.engine_for(&route)?;
        let mut engine = engine.lock().map_err(|e| {
            McpError::internal_error(
                format!(
                    "engine lock poisoned for room '{}' channel '{}': {e}",
                    route.room_id, route.channel_id
                ),
                None,
            )
        })?;

        match engine.process(data) {
            Ok(response) => {
                let text = serde_json::to_string_pretty(&response).unwrap_or_default();
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(msg) => Ok(Self::tool_error(msg)),
        }
    }
}

#[rmcp::tool_handler]
impl ServerHandler for StepwiseServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "kp-stepwise".into(),
                title: Some("Stepwise Planning MCP Server".into()),
                version: env!("CARGO_PKG_VERSION").into(),
                description: None,
                icons: None,
                website_url: None,
            },
            capabilities: rmcp::model::ServerCapabilities {
                tools: Some(rmcp::model::ToolsCapability {
                    list_changed: Some(true),
                }),
                resources: Some(rmcp::model::ResourcesCapability::default()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_
    {
        use rmcp::model::{Annotated, RawResourceTemplate};

        let templates = vec![
            Annotated::new(
                RawResourceTemplate {
                    uri_template: "stepwise://rooms/{roomId}/channels".into(),
                    name: "room_channels".into(),
                    title: Some("Room Channels".into()),
                    description: Some(
                        "Lists independently isolated planner channels in a room".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                None,
            ),
            Annotated::new(
                RawResourceTemplate {
                    uri_template:
                        "stepwise://rooms/{roomId}/channels/{channelId}/steps".into(),
                    name: "channel_steps".into(),
                    title: Some("Channel Steps".into()),
                    description: Some(
                        "Returns one channel's external checkpoint history as compressed JSON".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                None,
            ),
            Annotated::new(
                RawResourceTemplate {
                    uri_template:
                        "stepwise://rooms/{roomId}/channels/{channelId}/session".into(),
                    name: "channel_session".into(),
                    title: Some("Channel Metadata".into()),
                    description: Some(
                        "Returns room/channel identity, profile, logging, and expected next checkpoint"
                            .into(),
                    ),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                None,
            ),
            Annotated::new(
                RawResourceTemplate {
                    uri_template:
                        "stepwise://rooms/{roomId}/channels/{channelId}/branches".into(),
                    name: "channel_branches".into(),
                    title: Some("Channel Branches".into()),
                    description: Some(
                        "Returns branch names and step counts for one isolated channel".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                None,
            ),
            Annotated::new(
                RawResourceTemplate {
                    uri_template:
                        "stepwise://rooms/{roomId}/channels/{channelId}/stats".into(),
                    name: "channel_stats".into(),
                    title: Some("Channel Usage Stats".into()),
                    description: Some(
                        "Returns confidence and branching heuristics for one isolated channel".into(),
                    ),
                    mime_type: Some("application/json".into()),
                    icons: None,
                },
                None,
            ),
        ];

        std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(templates)))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = &request.uri;
        let path = uri.strip_prefix("stepwise://rooms/").ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "Unknown resource URI: {uri}. Use stepwise://rooms/{{roomId}}/channels/..."
                ),
                None,
            )
        })?;
        let segments: Vec<&str> = path.split('/').collect();

        if segments.len() == 2 && segments[1] == "channels" {
            let room_id = segments[0];
            Self::validate_route_id("roomId", room_id)
                .map_err(|message| McpError::invalid_params(message, None))?;

            let channel_engines: Vec<(String, SharedEngine)> = {
                let engines = self.engines.lock().map_err(|e| {
                    McpError::internal_error(format!("engine registry lock poisoned: {e}"), None)
                })?;
                engines
                    .iter()
                    .filter(|(route, _)| route.room_id == room_id)
                    .map(|(route, engine)| (route.channel_id.clone(), engine.clone()))
                    .collect()
            };

            let mut channels = Vec::with_capacity(channel_engines.len());
            for (channel_id, engine) in channel_engines {
                let engine = engine.lock().map_err(|e| {
                    McpError::internal_error(
                        format!(
                            "engine lock poisoned for room '{room_id}' channel '{channel_id}': {e}"
                        ),
                        None,
                    )
                })?;
                channels.push(engine.session_info());
            }
            channels.sort_by(|a, b| a["channelId"].as_str().cmp(&b["channelId"].as_str()));

            let json_text = serde_json::to_string(&serde_json::json!({
                "roomId": room_id,
                "channelCount": channels.len(),
                "channels": channels,
            }))
            .unwrap_or_default();

            return Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(json_text, &request.uri)],
            });
        }

        if segments.len() != 4 || segments[1] != "channels" {
            return Err(McpError::invalid_params(
                format!(
                    "Unknown resource URI: {uri}. Expected stepwise://rooms/{{roomId}}/channels or stepwise://rooms/{{roomId}}/channels/{{channelId}}/{{steps|branches|stats|session}}"
                ),
                None,
            ));
        }

        let route = ChannelRoute {
            room_id: segments[0].to_string(),
            channel_id: segments[2].to_string(),
        };
        Self::validate_route_id("roomId", &route.room_id)
            .and_then(|_| Self::validate_route_id("channelId", &route.channel_id))
            .map_err(|message| McpError::invalid_params(message, None))?;

        let engine = self.existing_engine(&route)?.ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "Unknown stepwise channel: room '{}' channel '{}'",
                    route.room_id, route.channel_id
                ),
                None,
            )
        })?;
        let engine = engine.lock().map_err(|e| {
            McpError::internal_error(
                format!(
                    "engine lock poisoned for room '{}' channel '{}': {e}",
                    route.room_id, route.channel_id
                ),
                None,
            )
        })?;

        let json_text = match segments[3] {
            "steps" => {
                let history = engine.step_history();
                let compact: Vec<serde_json::Value> = history
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "n": t.step_number,
                            "total": t.total_steps,
                            "step": t.step,
                            "confidence": t.confidence,
                            "branch": t.branch_id,
                            "layer": t.layer,
                            "mode": t.continuation_mode,
                            "turnId": t.turn_id,
                            "kind": t.checkpoint_kind,
                            "evidence": t.evidence,
                            "openQuestions": t.open_questions,
                            "nextAction": t.next_action,
                        })
                    })
                    .collect();
                serde_json::to_string(&compact).unwrap_or_default()
            }
            "branches" => {
                let branches = engine.branches();
                let summary: serde_json::Value = branches
                    .iter()
                    .map(|(name, steps)| (name.clone(), serde_json::json!(steps.len())))
                    .collect::<serde_json::Map<String, serde_json::Value>>()
                    .into();
                serde_json::to_string(&summary).unwrap_or_default()
            }
            "stats" => {
                let stats = engine.usage_stats();
                serde_json::to_string(&stats).unwrap_or_default()
            }
            "session" => serde_json::to_string(&engine.session_info()).unwrap_or_default(),
            other => {
                return Err(McpError::invalid_params(
                    format!(
                        "Unknown channel resource path: {other}. Expected steps, branches, stats, or session"
                    ),
                    None,
                ));
            }
        };

        Ok(ReadResourceResult {
            contents: vec![ResourceContents::text(json_text, &request.uri)],
        })
    }
}

pub async fn run() -> anyhow::Result<()> {
    let model_id = std::env::var("STEPWISE_MODEL")
        .or_else(|_| std::env::var("SEQUENTIAL_THINKING_MODEL")) // legacy name
        .unwrap_or_else(|_| "unknown".into());
    let client_type = detect_client_type();

    let all_profiles = profiles::load_profiles();
    let profile = profiles::get_profile_for_model(&model_id, &all_profiles);

    tracing::info!(
        model = %model_id,
        profile = %profile.display_name,
        client = %client_type,
        "stepwise planning server ready"
    );

    let session_id = PersistentLogger::resolve_session_id();
    let server = StepwiseServer::new(profile, model_id, client_type, session_id);

    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

fn detect_client_type() -> String {
    if std::env::var("CODEX_THREAD_ID").is_ok() {
        "codex".into()
    } else if std::env::var("CLAUDE_CODE_VERSION").is_ok()
        || std::env::var("CLAUDE_AGENT_SDK").is_ok()
    {
        "claude-code".into()
    } else if std::env::var("GEMINI_CLI").is_ok() || std::env::var("GOOGLE_CLI").is_ok() {
        "gemini-cli".into()
    } else if std::env::var("TALENTOS_AGENT").is_ok() {
        "talentos".into()
    } else {
        "unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_params(step: &str, num: u32, total: u32) -> StepwisePlanParams {
        StepwisePlanParams {
            channel_id: "test-main".into(),
            room_id: Some("test-room".into()),
            step: step.into(),
            step_number: num,
            total_steps: total,
            next_step_needed: None,
            turn_id: None,
            checkpoint_kind: None,
            evidence: None,
            open_questions: None,
            next_action: None,
            is_revision: None,
            revises_step: None,
            branch_from_step: None,
            branch_id: None,
            needs_more_steps: None,
            continuation_mode: None,
            explore_count: None,
            proposals: None,
            layer: None,
            delegate_to_next_layer: None,
            branch_strategy: None,
            merge_branches: None,
            confidence: None,
            done_reason: None,
            context_window: None,
            search_query: None,
            search_context: None,
            incorporate_search: None,
        }
    }

    fn make_server() -> StepwiseServer {
        std::env::set_var("DISABLE_STEP_LOGGING", "true");
        let profile = crate::profiles::fallback_profile();
        StepwiseServer::new(
            profile,
            "test-model".into(),
            "test-client".into(),
            "test-session".into(),
        )
    }

    // ---- From<StepwisePlanParams> for StepData ----

    #[test]
    fn params_to_step_data_required_fields() {
        let params = make_params("hello", 1, 5);
        let data: StepData = params.into();
        assert_eq!(data.step, "hello");
        assert_eq!(data.step_number, 1);
        assert_eq!(data.total_steps, 5);
        // next_step_needed defaults to true when None
        assert!(data.next_step_needed);
    }

    #[test]
    fn params_to_step_data_all_optional_fields() {
        let params = StepwisePlanParams {
            channel_id: "test-main".into(),
            room_id: Some("test-room".into()),
            step: "test".into(),
            step_number: 2,
            total_steps: 10,
            next_step_needed: Some(false),
            turn_id: Some("turn-2".into()),
            checkpoint_kind: Some("decision".into()),
            evidence: Some(vec!["test output".into()]),
            open_questions: Some(vec!["rollout risk".into()]),
            next_action: Some("implement".into()),
            is_revision: Some(true),
            revises_step: Some(1),
            branch_from_step: Some(1),
            branch_id: Some("branch-x".into()),
            needs_more_steps: Some(true),
            continuation_mode: Some("explore".into()),
            explore_count: Some(3),
            proposals: Some(vec!["A".into(), "B".into()]),
            layer: Some(2),
            delegate_to_next_layer: Some(true),
            branch_strategy: Some("parallel".into()),
            merge_branches: Some(vec!["branch-a".into(), "branch-b".into()]),
            confidence: Some(0.8),
            done_reason: Some("complete".into()),
            context_window: Some("expanded".into()),
            search_query: Some("query".into()),
            search_context: Some("context".into()),
            incorporate_search: Some(true),
        };
        let data: StepData = params.into();
        assert!(!data.next_step_needed);
        assert_eq!(data.turn_id.as_deref(), Some("turn-2"));
        assert_eq!(data.checkpoint_kind.as_deref(), Some("decision"));
        assert_eq!(data.evidence.as_ref().unwrap(), &["test output"]);
        assert_eq!(data.open_questions.as_ref().unwrap(), &["rollout risk"]);
        assert_eq!(data.next_action.as_deref(), Some("implement"));
        assert_eq!(data.is_revision, Some(true));
        assert_eq!(data.revises_step, Some(1));
        assert_eq!(data.branch_from_step, Some(1));
        assert_eq!(data.branch_id.as_deref(), Some("branch-x"));
        assert_eq!(data.needs_more_steps, Some(true));
        assert_eq!(data.continuation_mode.as_deref(), Some("explore"));
        assert_eq!(data.explore_count, Some(3));
        assert_eq!(data.proposals.as_ref().unwrap().len(), 2);
        assert_eq!(data.layer, Some(2));
        assert_eq!(data.delegate_to_next_layer, Some(true));
        assert_eq!(data.branch_strategy.as_deref(), Some("parallel"));
        assert_eq!(data.merge_branches.as_ref().unwrap().len(), 2);
        assert_eq!(data.confidence, Some(0.8));
        assert_eq!(data.done_reason.as_deref(), Some("complete"));
        assert_eq!(data.context_window.as_deref(), Some("expanded"));
        assert_eq!(data.search_query.as_deref(), Some("query"));
        assert_eq!(data.search_context.as_deref(), Some("context"));
        assert_eq!(data.incorporate_search, Some(true));
    }

    // ---- stepwise_plan tool method ----

    /// Extract the text string from the first content item of a CallToolResult.
    fn extract_text(result: &CallToolResult) -> String {
        result.content[0]
            .raw
            .as_text()
            .expect("expected text content")
            .text
            .clone()
    }

    #[test]
    fn tool_method_valid_step_returns_success() {
        let server = make_server();
        let params = make_params("Analyzing the problem", 1, 5);
        let result = server.stepwise_plan(Parameters(params));
        assert!(result.is_ok());
        let call_result = result.unwrap();
        assert!(!call_result.is_error.unwrap_or(false));
        let text = extract_text(&call_result);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["stepNumber"], 1);
        assert_eq!(parsed["totalSteps"], 5);
        assert!(parsed["sessionId"].is_string());
        assert_eq!(parsed["roomId"], "test-room");
        assert_eq!(parsed["channelId"], "test-main");
        assert!(parsed["logMode"].is_string());
        assert!(parsed.get("firstCallGuidance").is_some());
    }

    #[test]
    fn tool_method_invalid_step_returns_error() {
        let server = make_server();
        let params = make_params("", 1, 5); // empty step = invalid
        let result = server.stepwise_plan(Parameters(params));
        assert!(result.is_ok()); // MCP errors are returned as OK with error content
        let call_result = result.unwrap();
        assert!(call_result.is_error.unwrap_or(false));
        let text = extract_text(&call_result);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(parsed.get("error").is_some());
        assert_eq!(parsed["status"], "failed");
    }

    #[test]
    fn tool_method_sequential_steps_accumulate_state() {
        let server = make_server();
        // First step
        let p1 = make_params("Step 1", 1, 5);
        let r1 = server.stepwise_plan(Parameters(p1)).unwrap();
        assert!(!r1.is_error.unwrap_or(false));

        // Second step
        let p2 = make_params("Step 2", 2, 5);
        let r2 = server.stepwise_plan(Parameters(p2)).unwrap();
        let text2 = extract_text(&r2);
        let parsed2: serde_json::Value = serde_json::from_str(&text2).unwrap();
        assert_eq!(parsed2["stepCount"], 2);
        assert!(parsed2.get("firstCallGuidance").is_none());

        // Third step with branch
        let mut p3 = make_params("Branch from step 1", 3, 5);
        p3.branch_from_step = Some(1);
        p3.branch_id = Some("alt-path".into());
        let r3 = server.stepwise_plan(Parameters(p3)).unwrap();
        let text3 = extract_text(&r3);
        let parsed3: serde_json::Value = serde_json::from_str(&text3).unwrap();
        assert_eq!(parsed3["stepCount"], 3);
        let branches = parsed3["branches"].as_array().unwrap();
        assert!(branches.iter().any(|b| b.as_str() == Some("alt-path")));
    }

    #[test]
    fn parallel_channels_have_independent_histories_and_counters() {
        let server = make_server();

        let mut agent_a = make_params("Agent A first", 1, 3);
        agent_a.channel_id = "agent-a".into();
        agent_a.confidence = Some(0.2);
        let result_a = server.stepwise_plan(Parameters(agent_a)).unwrap();
        let parsed_a: serde_json::Value = serde_json::from_str(&extract_text(&result_a)).unwrap();

        let mut agent_b = make_params("Agent B first", 1, 3);
        agent_b.channel_id = "agent-b".into();
        agent_b.confidence = Some(0.9);
        let result_b = server.stepwise_plan(Parameters(agent_b)).unwrap();
        let parsed_b: serde_json::Value = serde_json::from_str(&extract_text(&result_b)).unwrap();

        assert_eq!(parsed_a["stepCount"], 1);
        assert_eq!(parsed_b["stepCount"], 1);
        assert_eq!(parsed_a["channelId"], "agent-a");
        assert_eq!(parsed_b["channelId"], "agent-b");
        assert_eq!(parsed_a["usageStats"]["lowConfWithoutBranchCount"], 1);
        assert_eq!(parsed_b["usageStats"]["lowConfWithoutBranchCount"], 0);
        assert_eq!(server.engines.lock().unwrap().len(), 2);
    }

    #[test]
    fn invalid_route_ids_are_rejected_without_creating_state() {
        let server = make_server();
        let mut params = make_params("Bad route", 1, 2);
        params.channel_id = "../agent".into();

        let result = server.stepwise_plan(Parameters(params)).unwrap();
        assert!(result.is_error.unwrap_or(false));
        let parsed: serde_json::Value = serde_json::from_str(&extract_text(&result)).unwrap();
        assert!(parsed["error"]
            .as_str()
            .unwrap()
            .contains("channelId may contain only"));
        assert!(server.engines.lock().unwrap().is_empty());
    }

    // ---- get_info ----

    #[test]
    fn server_info_has_correct_name() {
        let server = make_server();
        let info = server.get_info();
        assert_eq!(info.server_info.name, "kp-stepwise");
        assert!(info
            .server_info
            .title
            .as_deref()
            .unwrap()
            .contains("Stepwise Planning"));
    }

    // ---- detect_client_type ----
    // NOTE: env var tests must be serialized to avoid races. We test all
    // branches in a single test to avoid parallel env var conflicts.

    #[test]
    fn tool_method_poisoned_lock_returns_error() {
        let server = make_server();
        // Poison the registry mutex by panicking inside a lock.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = server.engines.lock().unwrap();
            panic!("intentional panic to poison the mutex");
        }));
        // Now the mutex is poisoned
        let params = make_params("This should fail", 1, 5);
        let result = server.stepwise_plan(Parameters(params));
        assert!(result.is_err()); // McpError from poisoned lock
    }

    // ---- resource tests ----

    #[test]
    fn server_info_has_resources_capability() {
        let server = make_server();
        let info = server.get_info();
        assert!(info.capabilities.resources.is_some());
    }

    // ---- engine getter tests (used by resource handlers) ----

    #[test]
    fn engine_step_history_empty() {
        let engine = crate::planner::PlanEngine::new(
            crate::profiles::fallback_profile(),
            "test".into(),
            "test".into(),
        );
        assert!(engine.step_history().is_empty());
    }

    #[test]
    fn engine_step_history_after_processing() {
        std::env::set_var("DISABLE_STEP_LOGGING", "true");
        let mut engine = crate::planner::PlanEngine::new(
            crate::profiles::fallback_profile(),
            "test".into(),
            "test".into(),
        );
        let data: StepData = make_params("First step", 1, 3).into();
        engine.process(data).unwrap();
        let data2: StepData = make_params("Second step", 2, 3).into();
        engine.process(data2).unwrap();
        assert_eq!(engine.step_history().len(), 2);
        assert_eq!(engine.step_history()[0].step_number, 1);
        assert_eq!(engine.step_history()[1].step_number, 2);
    }

    #[test]
    fn engine_branches_after_branching() {
        std::env::set_var("DISABLE_STEP_LOGGING", "true");
        let mut engine = crate::planner::PlanEngine::new(
            crate::profiles::fallback_profile(),
            "test".into(),
            "test".into(),
        );
        let data: StepData = make_params("Start", 1, 5).into();
        engine.process(data).unwrap();

        let mut p2 = make_params("Branch A", 2, 5);
        p2.branch_from_step = Some(1);
        p2.branch_id = Some("branch-a".into());
        let data2: StepData = p2.into();
        engine.process(data2).unwrap();

        let branches = engine.branches();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches["branch-a"].len(), 1);
    }

    #[test]
    fn engine_usage_stats() {
        std::env::set_var("DISABLE_STEP_LOGGING", "true");
        let mut engine = crate::planner::PlanEngine::new(
            crate::profiles::fallback_profile(),
            "test".into(),
            "test".into(),
        );
        let data: StepData = make_params("Think", 1, 5).into();
        engine.process(data).unwrap();
        let stats = engine.usage_stats();
        assert_eq!(stats.consecutive_linear_steps, 1);
        assert!(!stats.needs_branching);
        assert!(!stats.explore_count_used);
    }

    #[test]
    fn detect_client_type_all_branches() {
        // Clear all detection env vars first
        std::env::remove_var("CODEX_THREAD_ID");
        std::env::remove_var("CLAUDE_CODE_VERSION");
        std::env::remove_var("CLAUDE_AGENT_SDK");
        std::env::remove_var("GEMINI_CLI");
        std::env::remove_var("GOOGLE_CLI");
        std::env::remove_var("TALENTOS_AGENT");

        // Unknown
        assert_eq!(detect_client_type(), "unknown");

        // Codex has highest priority
        std::env::set_var("CODEX_THREAD_ID", "thread-1");
        assert_eq!(detect_client_type(), "codex");
        std::env::remove_var("CODEX_THREAD_ID");

        // Talentos (test last-priority first so higher-priority vars don't interfere)
        std::env::set_var("TALENTOS_AGENT", "1");
        assert_eq!(detect_client_type(), "talentos");
        std::env::remove_var("TALENTOS_AGENT");

        // Gemini CLI via GEMINI_CLI
        std::env::set_var("GEMINI_CLI", "1");
        assert_eq!(detect_client_type(), "gemini-cli");
        std::env::remove_var("GEMINI_CLI");

        // Gemini CLI via GOOGLE_CLI
        std::env::set_var("GOOGLE_CLI", "1");
        assert_eq!(detect_client_type(), "gemini-cli");
        std::env::remove_var("GOOGLE_CLI");

        // Claude Code via CLAUDE_CODE_VERSION
        std::env::set_var("CLAUDE_CODE_VERSION", "1.0");
        assert_eq!(detect_client_type(), "claude-code");
        std::env::remove_var("CLAUDE_CODE_VERSION");

        // Claude Code via CLAUDE_AGENT_SDK
        std::env::set_var("CLAUDE_AGENT_SDK", "1");
        assert_eq!(detect_client_type(), "claude-code");
        std::env::remove_var("CLAUDE_AGENT_SDK");
    }
}
