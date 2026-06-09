use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::logging::PersistentLogger;
use crate::profiles::TuningProfile;

// ============================================================================
// StepData — all fields from the TS interface
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepData {
    // Required
    pub step: String,
    pub step_number: u32,
    pub total_steps: u32,
    #[serde(default)]
    pub next_step_needed: bool,

    // Original optional (promoted to first-class)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_revision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revises_step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_from_step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_more_steps: Option<bool>,

    // Wide exploration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explore_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposals: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegate_to_next_layer: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_branches: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<String>,

    // Search-aware
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incorporate_search: Option<bool>,
}

// ============================================================================
// Usage tracking
// ============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub consecutive_linear_steps: u32,
    pub low_conf_without_branch_count: u32,
    pub explore_count_used: bool,
    pub needs_branching: bool,
}

/// A non-prescriptive hint the server surfaces. The caller decides what to do.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hint {
    /// Machine-readable hint kind for programmatic use
    pub kind: String,
    /// Human-readable suggestion
    pub message: String,
    /// How notable this observation is: "info", "suggestion", "observation"
    pub severity: String,
    /// Optional structured metadata (only present for spawn_candidate hints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_meta: Option<SpawnMeta>,
}

/// Metadata for a spawn_candidate hint. Callers use this to decide
/// whether and how to spawn subagents for parallel exploration.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnMeta {
    /// Branch IDs that could be explored in parallel by subagents
    pub branch_points: Vec<String>,
    /// Suggested planning depth for spawned agents (based on remaining steps)
    pub recommended_depth: u32,
    /// Suggested model tier: "same", "cheaper", "stronger" based on confidence and layer
    pub recommended_model: String,
}

/// Per-branch outcome extracted during merge.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchOutcome {
    /// Branch identifier
    pub branch_id: String,
    /// Confidence of the last step in this branch (None if never set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_confidence: Option<f64>,
    /// Done reason from the last step (None if branch didn't conclude)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_reason: Option<String>,
    /// Number of steps in this branch
    pub step_count: usize,
}

/// Summary of a branch merge operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSummary {
    pub merged_branches: Vec<String>,
    pub step_counts: HashMap<String, usize>,
    pub missing_branches: Vec<String>,
    /// Per-branch outcomes with final confidence and done_reason
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_outcomes: Option<Vec<BranchOutcome>>,
    /// Whether branches agreed: "converged" (confidences within 0.2), "diverged" (spread > 0.4), "mixed", or "insufficient" (< 2 branches with confidence)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub convergence_signal: Option<String>,
}

// ============================================================================
// PlanEngine — core processing logic
// ============================================================================

pub struct PlanEngine {
    step_history: Vec<StepData>,
    branches: HashMap<String, Vec<StepData>>,
    profile: TuningProfile,
    #[allow(dead_code)] // Stored for future per-model analytics
    model_id: String,
    #[allow(dead_code)] // Stored for future per-client analytics
    client_type: String,
    disable_logging: bool,
    logger: PersistentLogger,

    // Usage counters
    consecutive_linear_steps: u32,
    low_conf_without_branch_count: u32,
    explore_count_usage_count: u32,
}

impl PlanEngine {
    pub fn new(profile: TuningProfile, model_id: String, client_type: String) -> Self {
        let disable_logging = std::env::var("DISABLE_STEP_LOGGING")
            .or_else(|_| std::env::var("DISABLE_THOUGHT_LOGGING")) // legacy name
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let logger = PersistentLogger::new(&model_id, &client_type, &profile.display_name);

        Self {
            step_history: Vec::new(),
            branches: HashMap::new(),
            profile,
            model_id,
            client_type,
            disable_logging,
            logger,
            consecutive_linear_steps: 0,
            low_conf_without_branch_count: 0,
            explore_count_usage_count: 0,
        }
    }

    #[allow(dead_code)] // Public API for future use
    pub fn profile(&self) -> &TuningProfile {
        &self.profile
    }

    #[allow(dead_code)] // Accessor for future diagnostics/tests
    pub(crate) fn step_history(&self) -> &[StepData] {
        &self.step_history
    }

    #[allow(dead_code)] // Accessor for future diagnostics/tests
    pub(crate) fn branches(&self) -> &HashMap<String, Vec<StepData>> {
        &self.branches
    }

    #[allow(dead_code)] // Accessor for future diagnostics/tests
    pub(crate) fn usage_stats(&self) -> UsageStats {
        UsageStats {
            consecutive_linear_steps: self.consecutive_linear_steps,
            low_conf_without_branch_count: self.low_conf_without_branch_count,
            explore_count_used: self.explore_count_usage_count > 0,
            needs_branching: self.consecutive_linear_steps >= 4,
        }
    }

    /// Validate and clamp input fields, returning a clean StepData.
    fn validate(&self, mut data: StepData) -> Result<StepData, String> {
        if data.step.is_empty() {
            return Err("Invalid step: must be a non-empty string".into());
        }
        if data.step_number == 0 {
            return Err("Invalid stepNumber: must be >= 1".into());
        }
        if data.total_steps == 0 {
            return Err("Invalid totalSteps: must be >= 1".into());
        }

        // Derive nextStepNeeded from continuationMode if not explicitly set
        if let Some(ref mode) = data.continuation_mode {
            // If the caller provided continuationMode, use it to derive nextStepNeeded
            data.next_step_needed = mode != "done";
        }
        // If neither continuationMode nor a meaningful nextStepNeeded — default true
        // (backwards compat: the TS version required one or the other, but we're lenient)

        // Clamp exploreCount
        if let Some(ref mut ec) = data.explore_count {
            *ec = (*ec).clamp(1, self.profile.max_explore_count);
        }

        // Clamp confidence
        if let Some(ref mut c) = data.confidence {
            *c = c.clamp(0.0, 1.0);
        }

        // Clamp layer
        if let Some(ref mut l) = data.layer {
            *l = (*l).clamp(1, 5);
        }

        // Auto-adjust totalSteps if exceeded
        if data.step_number > data.total_steps {
            data.total_steps = data.step_number;
        }

        Ok(data)
    }

    /// Format a step for stderr display (compact single-line).
    fn format_step(&self, data: &StepData) -> String {
        let mut parts = Vec::new();

        // Prefix
        if data.is_revision.unwrap_or(false) {
            parts.push(format!(
                ">> Revision {}/{}",
                data.step_number, data.total_steps
            ));
        } else if data.branch_from_step.is_some() {
            parts.push(format!(
                "~> Branch {}/{} (from {}, {})",
                data.step_number,
                data.total_steps,
                data.branch_from_step.unwrap_or(0),
                data.branch_id.as_deref().unwrap_or("?")
            ));
        } else {
            parts.push(format!(
                ".. Step {}/{}",
                data.step_number, data.total_steps
            ));
        }

        // Extras inline
        if let Some(layer) = data.layer {
            parts.push(format!("L{}", layer));
        }
        if let Some(conf) = data.confidence {
            parts.push(format!("{}%", (conf * 100.0).round() as u32));
        }
        if let Some(ref mode) = data.continuation_mode {
            if mode != "continue" {
                parts.push(mode.clone());
            }
        }

        // Truncated step preview
        let preview: String = data.step.chars().take(120).collect();
        let ellipsis = if data.step.len() > 120 { "..." } else { "" };

        format!("-- {} | {}{}", parts.join(" | "), preview, ellipsis)
    }

    /// Process a step and return the JSON response.
    pub fn process(
        &mut self,
        data: StepData,
    ) -> Result<serde_json::Value, String> {
        let validated = self.validate(data)?;

        self.step_history.push(validated.clone());

        // Persist to JSONL log
        self.logger.persist(&validated);

        // Track branches
        if let (Some(_from), Some(ref bid)) =
            (validated.branch_from_step, &validated.branch_id)
        {
            self.branches
                .entry(bid.clone())
                .or_default()
                .push(validated.clone());
            self.consecutive_linear_steps = 0;
        } else {
            self.consecutive_linear_steps += 1;
        }

        // Track explore_count usage
        if validated.explore_count.unwrap_or(0) > 1 {
            self.explore_count_usage_count += 1;
        }

        // Track low-confidence without branch
        if let Some(conf) = validated.confidence {
            if conf < self.profile.branching_threshold && validated.branch_from_step.is_none() {
                self.low_conf_without_branch_count += 1;
            }
        }

        // Formatted output to stderr
        if !self.disable_logging {
            let formatted = self.format_step(&validated);
            eprintln!("{}", formatted);
        }

        // Build hints (non-prescriptive observations)
        let mut hints: Vec<Hint> = Vec::new();

        // --- Hint: linear chain getting long ---
        if self.consecutive_linear_steps >= 4 {
            hints.push(Hint {
                kind: "linear_chain".into(),
                message: format!(
                    "{} consecutive linear steps. Branching (branchFromStep + branchId) is available if you want to explore alternatives.",
                    self.consecutive_linear_steps
                ),
                severity: "suggestion".into(),
                spawn_meta: None,
            });
        }

        // --- Hint: explore_count available ---
        if self.explore_count_usage_count == 0 && validated.step_number >= 3 {
            hints.push(Hint {
                kind: "explore_available".into(),
                message: "exploreCount is available but unused. Try: exploreCount: 4, proposals: [...] to widen exploration.".into(),
                severity: "info".into(),
                spawn_meta: None,
            });
        }

        // --- Hint: low confidence pattern ---
        if self.low_conf_without_branch_count >= 2 {
            hints.push(Hint {
                kind: "low_confidence_pattern".into(),
                message: format!(
                    "{} low-confidence steps without branching. Branching can help validate uncertain conclusions.",
                    self.low_conf_without_branch_count
                ),
                severity: "suggestion".into(),
                spawn_meta: None,
            });
        }

        // --- Hint: Dunning-Kruger detection (high confidence at layer 1) ---
        if let (Some(conf), Some(layer)) = (validated.confidence, validated.layer) {
            if conf > 0.8 && layer <= 1 && validated.step_number <= 2 {
                hints.push(Hint {
                    kind: "premature_confidence".into(),
                    message: format!(
                        "Confidence {:.0}% at layer {} on step {}. High confidence before deep analysis can indicate premature closure. Layer 2+ exploration may reveal unknowns.",
                        conf * 100.0, layer, validated.step_number
                    ),
                    severity: "observation".into(),
                    spawn_meta: None,
                });
            }
        }

        // --- Hint: confidence without layer tracking ---
        if validated.confidence.is_some() && validated.layer.is_none() && validated.step_number >= 2 {
            hints.push(Hint {
                kind: "layer_available".into(),
                message: "Confidence is tracked but layer is not set. Layers (1=problem, 2=approach, 3=details) help calibrate whether confidence is warranted at this stage.".into(),
                severity: "info".into(),
                spawn_meta: None,
            });
        }

        // --- Hint: merge available when exactly 2 branches exist ---
        // At 3+ branches, subagent_orchestration takes over with richer guidance.
        if self.branches.len() == 2
            && validated.continuation_mode.as_deref() != Some("merge")
            && validated.merge_branches.is_none()
        {
            let branch_names: Vec<String> = self.branches.keys().cloned().collect();
            hints.push(Hint {
                kind: "merge_available".into(),
                message: format!(
                    "{} branches exist ({}). Use continuation_mode: \"merge\" with merge_branches: [...] to synthesize insights.",
                    self.branches.len(),
                    branch_names.join(", ")
                ),
                severity: "info".into(),
                spawn_meta: None,
            });
        }

        // --- Hint: spawn_candidate when parallel exploration would help ---
        let spawn_candidate = {
            let is_wide_explore = validated.continuation_mode.as_deref() == Some("explore")
                && validated.explore_count.unwrap_or(0) >= 3
                && validated.proposals.as_ref().map_or(false, |p| p.len() >= 3);

            let has_uncertain_branches = self.branches.len() >= 2
                && self.branches.values().any(|steps| {
                    steps.last().map_or(false, |t| {
                        t.confidence.map_or(false, |c| c < self.profile.branching_threshold)
                    })
                });

            let is_branching_with_existing = validated.continuation_mode.as_deref() == Some("branch")
                && validated.branch_from_step.is_some()
                && self.branches.len() >= 2;

            is_wide_explore || has_uncertain_branches || is_branching_with_existing
        };

        if spawn_candidate {
            let branch_points: Vec<String> = if validated.continuation_mode.as_deref() == Some("explore") {
                // For explore mode, use proposal descriptions as branch point names
                validated.proposals.as_ref()
                    .map(|p| p.iter().enumerate().map(|(i, _desc)| {
                        format!("proposal-{}", i + 1)
                    }).collect())
                    .unwrap_or_default()
            } else {
                // For branch mode, use existing branch names
                self.branches.keys().cloned().collect()
            };

            let remaining = validated.total_steps.saturating_sub(validated.step_number);
            let recommended_depth = remaining.max(3).min(10);

            let recommended_model = if validated.confidence.unwrap_or(0.5) < 0.3 {
                "stronger".to_string()  // Very uncertain = use stronger model
            } else if validated.layer.unwrap_or(1) <= 1 {
                "same".to_string()  // Still at problem understanding = same model
            } else {
                "cheaper".to_string()  // Deeper layers with moderate confidence = cheaper OK
            };

            hints.push(Hint {
                kind: "spawn_candidate".into(),
                message: format!(
                    "Parallel exploration opportunity: {} branch points detected. \
                     Spawning subagents could explore these concurrently.",
                    branch_points.len()
                ),
                severity: "suggestion".into(),
                spawn_meta: Some(SpawnMeta {
                    branch_points,
                    recommended_depth,
                    recommended_model,
                }),
            });
        }

        // --- Hint: subagent spawn opportunity ---
        // When a branch is created AND the branch_strategy is "parallel" (or multiple
        // proposals exist), hint that the caller could spawn independent subagents to
        // explore branches concurrently, then merge results back.
        if validated.branch_from_step.is_some() && validated.branch_id.is_some() {
            let strategy = validated.branch_strategy.as_deref().unwrap_or("sequential");
            if strategy == "parallel" || (validated.proposals.as_ref().map_or(false, |p| p.len() >= 3)) {
                let branch_name = validated.branch_id.as_deref().unwrap_or("unknown");
                let proposal_count = validated.proposals.as_ref().map_or(0, |p| p.len());
                hints.push(Hint {
                    kind: "subagent_spawn_available".into(),
                    message: format!(
                        "Branch '{}' could be explored by an independent subagent. \
                         {} proposals identified. The caller can spawn an Agent tool with \
                         this branch's context, let it explore independently, then merge \
                         results back with continuation_mode: \"merge\", \
                         merge_branches: [\"{}\"].",
                        branch_name, proposal_count, branch_name
                    ),
                    severity: "suggestion".into(),
                    spawn_meta: None,
                });
            }
        }

        // --- Hint: multi-branch subagent orchestration ---
        // When 3+ branches exist, suggest spawning agents for each and merging
        if self.branches.len() >= 3
            && validated.continuation_mode.as_deref() != Some("merge")
        {
            let branch_names: Vec<String> = self.branches.keys().cloned().collect();
            hints.push(Hint {
                kind: "subagent_orchestration".into(),
                message: format!(
                    "{} branches exist. Consider spawning {} parallel subagents (one per branch: {}) \
                     to explore independently, then merge all results in a final step.",
                    self.branches.len(),
                    self.branches.len(),
                    branch_names.join(", ")
                ),
                severity: "suggestion".into(),
                spawn_meta: None,
            });
        }

        // Process merge if requested
        let merge_summary = if validated.continuation_mode.as_deref() == Some("merge") {
            if let Some(ref requested) = validated.merge_branches {
                let mut merged = Vec::new();
                let mut missing = Vec::new();
                let mut counts = HashMap::new();
                for branch_name in requested {
                    if let Some(steps) = self.branches.get(branch_name) {
                        counts.insert(branch_name.clone(), steps.len());
                        merged.push(branch_name.clone());
                    } else {
                        missing.push(branch_name.clone());
                    }
                }

                // Extract branch outcomes
                let mut outcomes: Vec<BranchOutcome> = Vec::new();
                for branch_name in &merged {
                    if let Some(steps) = self.branches.get(branch_name) {
                        let last = steps.last();
                        outcomes.push(BranchOutcome {
                            branch_id: branch_name.clone(),
                            final_confidence: last.and_then(|t| t.confidence),
                            done_reason: last.and_then(|t| t.done_reason.clone()),
                            step_count: steps.len(),
                        });
                    }
                }

                // Compute convergence signal
                let confidences: Vec<f64> = outcomes.iter()
                    .filter_map(|o| o.final_confidence)
                    .collect();
                let convergence_signal = if confidences.len() < 2 {
                    "insufficient".to_string()
                } else {
                    let min = confidences.iter().cloned().fold(f64::INFINITY, f64::min);
                    let max = confidences.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let spread = max - min;
                    if spread <= 0.2 {
                        "converged".to_string()
                    } else if spread > 0.4 {
                        "diverged".to_string()
                    } else {
                        "mixed".to_string()
                    }
                };

                Some(MergeSummary {
                    merged_branches: merged,
                    step_counts: counts,
                    missing_branches: missing,
                    branch_outcomes: Some(outcomes),
                    convergence_signal: Some(convergence_signal),
                })
            } else {
                // Merge all branches by default
                let mut counts = HashMap::new();
                let merged: Vec<String> = self.branches.keys().cloned().collect();
                for (name, steps) in &self.branches {
                    counts.insert(name.clone(), steps.len());
                }

                // Extract branch outcomes
                let mut outcomes: Vec<BranchOutcome> = Vec::new();
                for branch_name in &merged {
                    if let Some(steps) = self.branches.get(branch_name) {
                        let last = steps.last();
                        outcomes.push(BranchOutcome {
                            branch_id: branch_name.clone(),
                            final_confidence: last.and_then(|t| t.confidence),
                            done_reason: last.and_then(|t| t.done_reason.clone()),
                            step_count: steps.len(),
                        });
                    }
                }

                // Compute convergence signal
                let confidences: Vec<f64> = outcomes.iter()
                    .filter_map(|o| o.final_confidence)
                    .collect();
                let convergence_signal = if confidences.len() < 2 {
                    "insufficient".to_string()
                } else {
                    let min = confidences.iter().cloned().fold(f64::INFINITY, f64::min);
                    let max = confidences.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let spread = max - min;
                    if spread <= 0.2 {
                        "converged".to_string()
                    } else if spread > 0.4 {
                        "diverged".to_string()
                    } else {
                        "mixed".to_string()
                    }
                };

                Some(MergeSummary {
                    merged_branches: merged,
                    step_counts: counts,
                    missing_branches: Vec::new(),
                    branch_outcomes: Some(outcomes),
                    convergence_signal: Some(convergence_signal),
                })
            }
        } else {
            None
        };

        // Build response
        let branch_keys: Vec<String> = self.branches.keys().cloned().collect();
        let usage_stats = UsageStats {
            consecutive_linear_steps: self.consecutive_linear_steps,
            low_conf_without_branch_count: self.low_conf_without_branch_count,
            explore_count_used: self.explore_count_usage_count > 0,
            needs_branching: self.consecutive_linear_steps >= 4,
        };

        let mut response = serde_json::json!({
            "stepNumber": validated.step_number,
            "totalSteps": validated.total_steps,
            "nextStepNeeded": validated.next_step_needed,
            "branches": branch_keys,
            "stepCount": self.step_history.len(),
            "usageStats": usage_stats,
        });

        // Hints array — always present, may be empty
        if !hints.is_empty() {
            response["hints"] = serde_json::to_value(&hints).unwrap_or_default();
        }

        // First-call guidance
        if validated.step_number == 1 {
            response["firstCallGuidance"] = serde_json::Value::String(
                first_call_guidance(&self.profile),
            );
        }

        // Search query passthrough
        if let Some(ref sq) = validated.search_query {
            response["pendingSearchQuery"] = serde_json::Value::String(sq.clone());
            response["hint"] =
                serde_json::Value::String("Agent should execute search before next step".into());
        }

        // Merge summary
        if let Some(ref summary) = merge_summary {
            response["mergeSummary"] = serde_json::to_value(summary).unwrap_or_default();
        }

        // Confidence-based guidance (kept for backwards compat, also in hints)
        if let Some(conf) = validated.confidence {
            if conf >= self.profile.confidence_threshold {
                response["guidance"] = serde_json::Value::String(
                    "High confidence reached. Consider continuation_mode=\"done\" if answer is sufficient.".into(),
                );
            } else if conf < self.profile.branching_threshold {
                response["guidance"] = serde_json::Value::String(
                    "Low confidence. Consider branching to explore alternatives.".into(),
                );
            }
        }

        Ok(response)
    }
}

/// Generate compact decision-tree guidance returned on the first step.
/// Uses compressed tokens — every token earns its place.
fn first_call_guidance(profile: &TuningProfile) -> String {
    let bt = (profile.branching_threshold * 100.0).round() as u32;
    let ct = (profile.confidence_threshold * 100.0).round() as u32;

    format!(
        "-- planner [{dn}] --\n\
         \n\
         DECIDE(confidence):\n\
           <{bt}% → branch(branchFromStep+branchId) or explore(count:{de}-{me},proposals:[...])\n\
           {bt}-{ct}% → continue(layer++) or revise(revisesStep:N)\n\
           >{ct}% → done(reason:complete|sufficient)\n\
         \n\
         DECIDE(branches≥2):\n\
           converging → merge(mergeBranches:[ids])\n\
           diverging → branch deeper or delegate(delegateToNextLayer)\n\
         \n\
         ALWAYS: set confidence. Third option exists. Verify before assuming.",
        bt = bt,
        ct = ct,
        dn = profile.display_name,
        de = profile.default_explore_count,
        me = profile.max_explore_count,
    )
}

/// Generate tool description text (embedded in tool schema).
#[allow(dead_code)] // Available for dynamic tool description generation
pub fn tool_description(profile: &TuningProfile) -> String {
    let bt = (profile.branching_threshold * 100.0).round() as u32;
    let ct = (profile.confidence_threshold * 100.0).round() as u32;

    format!(
        "Stepwise planning for multi-step problem-solving with branching and exploration.\n\
         Branch <{bt}% | exit >{ct}% | modes: explore/branch/merge/continue/done | \
         {dn} explore:{de}-{me} budget:{tbm}x",
        bt = bt,
        ct = ct,
        de = profile.default_explore_count,
        me = profile.max_explore_count,
        dn = profile.display_name,
        tbm = profile.token_budget_multiplier,
    )
}

#[allow(dead_code)] // Kept for potential future formatting needs
pub(crate) fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.len() + word.len() + 1 <= max_width {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        } else {
            if !current.is_empty() {
                lines.push(current);
            }
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{fallback_profile, default_profiles, get_profile_for_model};

    fn make_engine() -> PlanEngine {
        std::env::set_var("DISABLE_STEP_LOGGING", "true");
        let profile = fallback_profile();
        PlanEngine::new(profile, "test-model".into(), "test-client".into())
    }

    fn make_step(num: u32, total: u32) -> StepData {
        StepData {
            step: format!("Step number {}", num),
            step_number: num,
            total_steps: total,
            next_step_needed: true,
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
            search_context: None,
            search_query: None,
            incorporate_search: None,
        }
    }

    // ---- validate tests ----

    #[test]
    fn validate_valid_input() {
        let engine = make_engine();
        let t = make_step(1, 5);
        let result = engine.validate(t);
        assert!(result.is_ok());
        let v = result.unwrap();
        assert_eq!(v.step_number, 1);
        assert_eq!(v.total_steps, 5);
    }

    #[test]
    fn validate_empty_step_rejected() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.step = String::new();
        let result = engine.validate(t);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("non-empty"));
    }

    #[test]
    fn validate_zero_step_number_rejected() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.step_number = 0;
        let result = engine.validate(t);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("stepNumber"));
    }

    #[test]
    fn validate_zero_total_steps_rejected() {
        let engine = make_engine();
        let mut t = make_step(1, 0);
        t.step_number = 1; // valid
        t.total_steps = 0;
        let result = engine.validate(t);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("totalSteps"));
    }

    #[test]
    fn validate_clamps_confidence() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(1.5);
        let v = engine.validate(t).unwrap();
        assert_eq!(v.confidence, Some(1.0));

        let mut t2 = make_step(1, 5);
        t2.confidence = Some(-0.5);
        let v2 = engine.validate(t2).unwrap();
        assert_eq!(v2.confidence, Some(0.0));
    }

    #[test]
    fn validate_clamps_layer() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.layer = Some(10);
        let v = engine.validate(t).unwrap();
        assert_eq!(v.layer, Some(5));

        let mut t2 = make_step(1, 5);
        t2.layer = Some(0);
        let v2 = engine.validate(t2).unwrap();
        assert_eq!(v2.layer, Some(1));
    }

    #[test]
    fn validate_clamps_explore_count() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.explore_count = Some(100);
        let v = engine.validate(t).unwrap();
        // fallback profile max_explore_count = 5
        assert_eq!(v.explore_count, Some(5));
    }

    #[test]
    fn validate_auto_adjusts_total_steps() {
        let engine = make_engine();
        let t = make_step(10, 5); // stepNumber > totalSteps
        let v = engine.validate(t).unwrap();
        assert_eq!(v.total_steps, 10);
    }

    #[test]
    fn validate_continuation_mode_done_sets_next_false() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.continuation_mode = Some("done".into());
        t.next_step_needed = true;
        let v = engine.validate(t).unwrap();
        assert!(!v.next_step_needed);
    }

    #[test]
    fn validate_continuation_mode_continue_sets_next_true() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.continuation_mode = Some("continue".into());
        t.next_step_needed = false;
        let v = engine.validate(t).unwrap();
        assert!(v.next_step_needed);
    }

    // ---- process tests ----

    #[test]
    fn process_returns_correct_structure() {
        let mut engine = make_engine();
        let t = make_step(1, 5);
        let result = engine.process(t).unwrap();
        assert_eq!(result["stepNumber"], 1);
        assert_eq!(result["totalSteps"], 5);
        assert!(result["nextStepNeeded"].is_boolean());
        assert!(result["branches"].is_array());
        assert!(result["usageStats"].is_object());
        assert_eq!(result["stepCount"], 1);
    }

    #[test]
    fn process_first_step_has_guidance() {
        let mut engine = make_engine();
        let t = make_step(1, 5);
        let result = engine.process(t).unwrap();
        assert!(result.get("firstCallGuidance").is_some());
        let guidance = result["firstCallGuidance"].as_str().unwrap();
        assert!(guidance.contains("-- planner ["));
    }

    #[test]
    fn process_second_step_no_first_call_guidance() {
        let mut engine = make_engine();
        engine.process(make_step(1, 5)).unwrap();
        let result = engine.process(make_step(2, 5)).unwrap();
        assert!(result.get("firstCallGuidance").is_none());
    }

    #[test]
    fn process_branch_tracking() {
        let mut engine = make_engine();
        engine.process(make_step(1, 5)).unwrap();

        let mut branch_step = make_step(2, 5);
        branch_step.branch_from_step = Some(1);
        branch_step.branch_id = Some("alternative-a".into());
        let result = engine.process(branch_step).unwrap();

        let branches = result["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 1);
        assert!(branches.iter().any(|b| b.as_str() == Some("alternative-a")));
    }

    #[test]
    fn process_usage_consecutive_linear() {
        let mut engine = make_engine();
        for i in 1..=5 {
            engine.process(make_step(i, 10)).unwrap();
        }
        let result = engine.process(make_step(6, 10)).unwrap();
        let usage_stats = &result["usageStats"];
        assert_eq!(usage_stats["consecutiveLinearSteps"], 6);
        assert_eq!(usage_stats["needsBranching"], true);
    }

    #[test]
    fn process_usage_resets_on_branch() {
        let mut engine = make_engine();
        for i in 1..=4 {
            engine.process(make_step(i, 10)).unwrap();
        }

        let mut branch = make_step(5, 10);
        branch.branch_from_step = Some(3);
        branch.branch_id = Some("reset-branch".into());
        let result = engine.process(branch).unwrap();
        assert_eq!(result["usageStats"]["consecutiveLinearSteps"], 0);
        assert_eq!(result["usageStats"]["needsBranching"], false);
    }

    #[test]
    fn process_low_confidence_tracking() {
        let mut engine = make_engine();
        // fallback profile branching_threshold = 0.6
        let mut t1 = make_step(1, 5);
        t1.confidence = Some(0.3);
        engine.process(t1).unwrap();

        let mut t2 = make_step(2, 5);
        t2.confidence = Some(0.4);
        let result = engine.process(t2).unwrap();
        assert_eq!(result["usageStats"]["lowConfWithoutBranchCount"], 2);
    }

    #[test]
    fn process_explore_count_used_flag() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.explore_count = Some(3);
        let result = engine.process(t).unwrap();
        assert_eq!(result["usageStats"]["exploreCountUsed"], true);
    }

    #[test]
    fn process_explore_count_not_used() {
        let mut engine = make_engine();
        let t = make_step(1, 5);
        let result = engine.process(t).unwrap();
        assert_eq!(result["usageStats"]["exploreCountUsed"], false);
    }

    #[test]
    fn process_search_query_passthrough() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.search_query = Some("how to branch".into());
        let result = engine.process(t).unwrap();
        assert_eq!(result["pendingSearchQuery"], "how to branch");
        assert!(result.get("hint").is_some());
    }

    #[test]
    fn process_done_mode() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.continuation_mode = Some("done".into());
        let result = engine.process(t).unwrap();
        assert_eq!(result["nextStepNeeded"], false);
    }

    #[test]
    fn process_high_confidence_guidance() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(0.9); // above 0.75 threshold
        let result = engine.process(t).unwrap();
        let guidance = result["guidance"].as_str().unwrap();
        assert!(guidance.contains("done"));
    }

    #[test]
    fn process_low_confidence_guidance() {
        let mut engine = make_engine();
        let mut t = make_step(2, 5); // not step 1, to avoid firstCallGuidance noise
        t.confidence = Some(0.3); // below 0.6 threshold
        // Need to process step 1 first
        engine.process(make_step(1, 5)).unwrap();
        let result = engine.process(t).unwrap();
        let guidance = result["guidance"].as_str().unwrap();
        assert!(guidance.contains("branching"));
    }

    // ---- wrap_text tests ----

    #[test]
    fn wrap_text_short_line() {
        let lines = wrap_text("hello world", 80);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wrap_text_wraps_long_line() {
        let lines = wrap_text("one two three four five", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            // Each line should be <= 10 chars (words permitting)
            assert!(line.len() <= 10 || !line.contains(' '));
        }
    }

    #[test]
    fn wrap_text_empty_returns_one_empty() {
        let lines = wrap_text("", 80);
        assert_eq!(lines, vec![""]);
    }

    // ---- format_step tests ----

    #[test]
    fn format_step_revision() {
        let engine = make_engine();
        let mut t = make_step(2, 5);
        t.is_revision = Some(true);
        t.revises_step = Some(1);
        let output = engine.format_step(&t);
        assert!(output.contains("Revision"));
    }

    #[test]
    fn format_step_branch() {
        let engine = make_engine();
        let mut t = make_step(2, 5);
        t.branch_from_step = Some(1);
        t.branch_id = Some("test-branch".into());
        let output = engine.format_step(&t);
        assert!(output.contains("Branch"));
        assert!(output.contains("test-branch"));
    }

    #[test]
    fn format_step_with_confidence() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(0.85);
        let output = engine.format_step(&t);
        assert!(output.contains("85%"));
    }

    #[test]
    fn format_step_low_confidence() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(0.3);
        let output = engine.format_step(&t);
        assert!(output.contains("30%"));
    }

    #[test]
    fn format_step_no_confidence() {
        let engine = make_engine();
        let t = make_step(1, 5);
        let output = engine.format_step(&t);
        // Compact format: no confidence = no confidence field
        assert!(output.contains("Step 1/5"));
        assert!(!output.contains("%"));
    }

    #[test]
    fn format_step_explore_mode() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.continuation_mode = Some("explore".into());
        t.explore_count = Some(4);
        let output = engine.format_step(&t);
        assert!(output.contains("explore"));
    }

    #[test]
    fn format_step_with_layer() {
        let engine = make_engine();
        let mut t = make_step(1, 5);
        t.layer = Some(2);
        let output = engine.format_step(&t);
        assert!(output.contains("L2"));
    }

    // ---- tool_description test ----

    #[test]
    fn tool_description_compact() {
        let profile = fallback_profile();
        let desc = tool_description(&profile);
        assert!(desc.contains("Stepwise planning"));
        assert!(desc.contains("Branch"));
        // Should be compact — no massive guidance blocks
        assert!(desc.lines().count() <= 5);
    }

    // ---- first_call_guidance test ----

    #[test]
    fn first_call_guidance_decision_tree() {
        let profile = fallback_profile();
        let guidance = first_call_guidance(&profile);
        assert!(guidance.contains("-- planner"));
        assert!(guidance.contains("DECIDE(confidence)"));
        assert!(guidance.contains("DECIDE(branches"));
        assert!(guidance.contains("done(reason:"));
        assert!(guidance.contains("branch(branchFromStep"));
        assert!(guidance.contains("merge(mergeBranches"));
        // Compact: decision tree, not essay
        assert!(guidance.lines().count() <= 12);
    }

    // ---- engine with specific profile ----

    #[test]
    fn engine_with_claude_profile() {
        std::env::set_var("DISABLE_STEP_LOGGING", "true");
        let profiles = default_profiles();
        let profile = get_profile_for_model("claude-3-opus", &profiles);
        let mut engine = PlanEngine::new(profile.clone(), "claude-3-opus".into(), "test".into());
        assert_eq!(engine.profile().display_name, "Claude");

        let mut t = make_step(1, 3);
        t.explore_count = Some(10);
        let result = engine.process(t).unwrap();
        // Claude max_explore_count = 5, so it should be clamped
        assert_eq!(result["stepNumber"], 1);
    }

    // ---- multiple branches ----

    #[test]
    fn multiple_branches_tracked() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("branch-a".into());
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("branch-b".into());
        let result = engine.process(b2).unwrap();

        let branches = result["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 2);
    }

    // ---- history length increments ----

    #[test]
    fn history_length_increments() {
        let mut engine = make_engine();
        for i in 1..=3 {
            let result = engine.process(make_step(i, 5)).unwrap();
            assert_eq!(result["stepCount"], i as u64);
        }
    }

    // ---- Tests with logging ENABLED (exercises stderr usage warning paths) ----

    /// Create an engine with logging enabled (DISABLE_STEP_LOGGING unset/false).
    fn make_engine_with_logging() -> PlanEngine {
        std::env::remove_var("DISABLE_STEP_LOGGING");
        let profile = fallback_profile();
        PlanEngine::new(profile, "test-model-logging".into(), "test-client".into())
    }

    #[test]
    fn process_with_logging_linear_chain_warning() {
        // Exercise lines 308-309 (eprintln of formatted step) and 312-313 (linear chain warning)
        let mut engine = make_engine_with_logging();
        for i in 1..=5 {
            let result = engine.process(make_step(i, 10)).unwrap();
            // After 4+ consecutive linear steps, usage stats should flag it
            if i >= 4 {
                assert_eq!(result["usageStats"]["needsBranching"], true);
            }
        }
        // The 5th step has consecutive_linear_steps=5 >= 4, so the warning path ran
        assert_eq!(engine.consecutive_linear_steps, 5);
    }

    #[test]
    fn process_with_logging_explore_count_nudge() {
        // Exercise lines 321-322 (explore_count nudge for step >= 3 without explore_count usage)
        let mut engine = make_engine_with_logging();
        // Process 3 steps without using explore_count
        for i in 1..=3 {
            engine.process(make_step(i, 5)).unwrap();
        }
        // explore_count_usage_count should still be 0
        assert_eq!(engine.explore_count_usage_count, 0);
    }

    #[test]
    fn process_with_logging_low_confidence_usage() {
        // Exercise lines 327-328 (low-confidence usage warning)
        let mut engine = make_engine_with_logging();

        // Submit 2+ low-confidence steps without branching
        let mut t1 = make_step(1, 5);
        t1.confidence = Some(0.3);
        engine.process(t1).unwrap();

        let mut t2 = make_step(2, 5);
        t2.confidence = Some(0.4);
        engine.process(t2).unwrap();

        assert_eq!(engine.low_conf_without_branch_count, 2);
    }

    #[test]
    fn process_with_logging_all_warnings_at_once() {
        // Trigger all three warning paths in a single engine run
        let mut engine = make_engine_with_logging();

        // 5 linear steps with low confidence and no explore_count
        for i in 1..=5 {
            let mut t = make_step(i, 10);
            t.confidence = Some(0.2); // below 0.6 branching threshold
            engine.process(t).unwrap();
        }

        // All three warning conditions met:
        assert!(engine.consecutive_linear_steps >= 4);       // linear chain
        assert_eq!(engine.explore_count_usage_count, 0);        // no explore_count used
        assert!(engine.low_conf_without_branch_count >= 2);     // low-conf without branch
    }

    // ---- hints system tests ----

    #[test]
    fn hints_empty_when_no_issues() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(0.7);
        t.layer = Some(2);
        let result = engine.process(t).unwrap();
        // First step shouldn't have linear chain or low-conf hints
        assert!(result.get("hints").is_none() || result["hints"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hints_linear_chain_suggestion() {
        let mut engine = make_engine();
        for i in 1..=5 {
            engine.process(make_step(i, 10)).unwrap();
        }
        let result = engine.process(make_step(6, 10)).unwrap();
        let hints = result["hints"].as_array().unwrap();
        assert!(hints.iter().any(|h| h["kind"] == "linear_chain"));
        // It's a suggestion, not a mandate
        assert!(hints.iter().all(|h| h["severity"] != "error"));
    }

    #[test]
    fn hints_premature_confidence_dunning_kruger() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(0.9);
        t.layer = Some(1);
        let result = engine.process(t).unwrap();
        let hints = result["hints"].as_array().unwrap();
        assert!(hints.iter().any(|h| h["kind"] == "premature_confidence"));
        // Check it's an observation, not enforcement
        let dk_hint = hints.iter().find(|h| h["kind"] == "premature_confidence").unwrap();
        assert_eq!(dk_hint["severity"], "observation");
    }

    #[test]
    fn no_premature_confidence_at_layer_2() {
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.confidence = Some(0.9);
        t.layer = Some(2); // Layer 2 = approach selection, high confidence OK
        let result = engine.process(t).unwrap();
        // Should not have premature_confidence hint at layer 2
        if let Some(hints) = result.get("hints") {
            let hints = hints.as_array().unwrap();
            assert!(!hints.iter().any(|h| h["kind"] == "premature_confidence"));
        }
    }

    #[test]
    fn hints_merge_available_with_multiple_branches() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("approach-a".into());
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("approach-b".into());
        let result = engine.process(b2).unwrap();

        let hints = result["hints"].as_array().unwrap();
        assert!(hints.iter().any(|h| h["kind"] == "merge_available"));
    }

    // ---- merge tests ----

    #[test]
    fn merge_branches_returns_summary() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("branch-a".into());
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("branch-b".into());
        engine.process(b2).unwrap();

        // Now merge
        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        merge.merge_branches = Some(vec!["branch-a".into(), "branch-b".into()]);
        let result = engine.process(merge).unwrap();

        let summary = &result["mergeSummary"];
        assert!(summary.is_object());
        let merged = summary["mergedBranches"].as_array().unwrap();
        assert_eq!(merged.len(), 2);
        assert!(summary["missingBranches"].as_array().unwrap().is_empty());
    }

    #[test]
    fn merge_with_missing_branch_reports_it() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("real-branch".into());
        engine.process(b1).unwrap();

        let mut merge = make_step(3, 10);
        merge.continuation_mode = Some("merge".into());
        merge.merge_branches = Some(vec!["real-branch".into(), "ghost-branch".into()]);
        let result = engine.process(merge).unwrap();

        let summary = &result["mergeSummary"];
        let missing = summary["missingBranches"].as_array().unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0], "ghost-branch");
    }

    #[test]
    fn merge_all_branches_when_none_specified() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("auto-a".into());
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("auto-b".into());
        engine.process(b2).unwrap();

        // Merge without specifying which branches — should merge all
        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        let result = engine.process(merge).unwrap();

        let summary = &result["mergeSummary"];
        let merged = summary["mergedBranches"].as_array().unwrap();
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn hints_layer_available_when_confidence_set_without_layer() {
        let mut engine = make_engine();
        engine.process(make_step(1, 5)).unwrap();
        let mut t = make_step(2, 5);
        t.confidence = Some(0.6);
        // No layer set
        let result = engine.process(t).unwrap();
        let hints = result["hints"].as_array().unwrap();
        assert!(hints.iter().any(|h| h["kind"] == "layer_available"));
    }

    // ---- spawn_candidate hint tests ----

    #[test]
    fn spawn_hint_on_wide_explore() {
        let mut engine = make_engine();
        let mut t = make_step(1, 10);
        t.continuation_mode = Some("explore".into());
        t.explore_count = Some(4);
        t.proposals = Some(vec![
            "Approach A".into(),
            "Approach B".into(),
            "Approach C".into(),
            "Approach D".into(),
        ]);
        t.confidence = Some(0.4);
        let result = engine.process(t).unwrap();
        let hints = result["hints"].as_array().unwrap();
        let spawn_hint = hints.iter().find(|h| h["kind"] == "spawn_candidate");
        assert!(spawn_hint.is_some(), "expected spawn_candidate hint on wide explore");
        let meta = &spawn_hint.unwrap()["spawnMeta"];
        assert!(meta.is_object(), "expected spawnMeta object");
        assert_eq!(meta["branchPoints"].as_array().unwrap().len(), 4);
        assert!(meta["recommendedDepth"].as_u64().unwrap() >= 3);
        assert!(meta["recommendedModel"].is_string());
    }

    #[test]
    fn spawn_hint_on_uncertain_branches() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        // Create two branches with low confidence
        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("opt-a".into());
        b1.confidence = Some(0.3); // below branching_threshold (0.6)
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("opt-b".into());
        b2.confidence = Some(0.4);
        let result = engine.process(b2).unwrap();

        let hints = result["hints"].as_array().unwrap();
        let spawn_hint = hints.iter().find(|h| h["kind"] == "spawn_candidate");
        assert!(spawn_hint.is_some(), "expected spawn_candidate on uncertain branches");
        let meta = &spawn_hint.unwrap()["spawnMeta"];
        let branch_points = meta["branchPoints"].as_array().unwrap();
        assert_eq!(branch_points.len(), 2);
    }

    #[test]
    fn spawn_hint_on_branching_with_existing() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("path-a".into());
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("path-b".into());
        engine.process(b2).unwrap();

        // Third branch triggers spawn_candidate
        let mut b3 = make_step(4, 10);
        b3.continuation_mode = Some("branch".into());
        b3.branch_from_step = Some(1);
        b3.branch_id = Some("path-c".into());
        let result = engine.process(b3).unwrap();

        let hints = result["hints"].as_array().unwrap();
        assert!(hints.iter().any(|h| h["kind"] == "spawn_candidate"),
            "expected spawn_candidate when branching with 2+ existing branches");
    }

    #[test]
    fn no_spawn_hint_on_simple_explore() {
        // explore_count < 3 should NOT trigger spawn_candidate
        let mut engine = make_engine();
        let mut t = make_step(1, 5);
        t.continuation_mode = Some("explore".into());
        t.explore_count = Some(2);
        t.proposals = Some(vec!["A".into(), "B".into()]);
        let result = engine.process(t).unwrap();
        if let Some(hints) = result.get("hints") {
            let hints = hints.as_array().unwrap();
            assert!(!hints.iter().any(|h| h["kind"] == "spawn_candidate"),
                "should NOT get spawn_candidate with only 2 proposals");
        }
    }

    #[test]
    fn spawn_hint_recommended_model_stronger_on_very_low_confidence() {
        let mut engine = make_engine();
        let mut t = make_step(1, 10);
        t.continuation_mode = Some("explore".into());
        t.explore_count = Some(4);
        t.proposals = Some(vec!["A".into(), "B".into(), "C".into(), "D".into()]);
        t.confidence = Some(0.2); // very low -> should recommend "stronger"
        let result = engine.process(t).unwrap();
        let hints = result["hints"].as_array().unwrap();
        let spawn_hint = hints.iter().find(|h| h["kind"] == "spawn_candidate").unwrap();
        assert_eq!(spawn_hint["spawnMeta"]["recommendedModel"], "stronger");
    }

    #[test]
    fn spawn_hint_recommended_depth_uses_remaining_steps() {
        let mut engine = make_engine();
        // Process step 1 first
        engine.process(make_step(1, 15)).unwrap();
        let mut t = make_step(2, 15); // 13 remaining, clamped to 10
        t.continuation_mode = Some("explore".into());
        t.explore_count = Some(3);
        t.proposals = Some(vec!["A".into(), "B".into(), "C".into()]);
        t.confidence = Some(0.4);
        let result = engine.process(t).unwrap();
        let hints = result["hints"].as_array().unwrap();
        let spawn_hint = hints.iter().find(|h| h["kind"] == "spawn_candidate").unwrap();
        let depth = spawn_hint["spawnMeta"]["recommendedDepth"].as_u64().unwrap();
        assert!(depth >= 3 && depth <= 10, "recommended_depth should be clamped 3-10, got {}", depth);
    }

    // ---- enhanced merge tests (branch outcomes + convergence) ----

    #[test]
    fn merge_includes_branch_outcomes() {
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("opt-a".into());
        b1.confidence = Some(0.7);
        b1.done_reason = Some("sufficient".into());
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("opt-b".into());
        b2.confidence = Some(0.8);
        b2.done_reason = Some("complete".into());
        engine.process(b2).unwrap();

        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        merge.merge_branches = Some(vec!["opt-a".into(), "opt-b".into()]);
        let result = engine.process(merge).unwrap();

        let summary = &result["mergeSummary"];
        let outcomes = summary["branchOutcomes"].as_array().unwrap();
        assert_eq!(outcomes.len(), 2);

        let opt_a = outcomes.iter().find(|o| o["branchId"] == "opt-a").unwrap();
        assert_eq!(opt_a["finalConfidence"], 0.7);
        assert_eq!(opt_a["doneReason"], "sufficient");

        let opt_b = outcomes.iter().find(|o| o["branchId"] == "opt-b").unwrap();
        assert_eq!(opt_b["finalConfidence"], 0.8);
        assert_eq!(opt_b["doneReason"], "complete");
    }

    #[test]
    fn merge_convergence_signal_converged() {
        // Two branches with similar confidence (spread <= 0.2) -> "converged"
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("a".into());
        b1.confidence = Some(0.75);
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("b".into());
        b2.confidence = Some(0.85); // spread = 0.1 <= 0.2
        engine.process(b2).unwrap();

        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        let result = engine.process(merge).unwrap();

        assert_eq!(result["mergeSummary"]["convergenceSignal"], "converged");
    }

    #[test]
    fn merge_convergence_signal_diverged() {
        // Two branches with very different confidence (spread > 0.4) -> "diverged"
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("a".into());
        b1.confidence = Some(0.3);
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("b".into());
        b2.confidence = Some(0.9); // spread = 0.6 > 0.4
        engine.process(b2).unwrap();

        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        let result = engine.process(merge).unwrap();

        assert_eq!(result["mergeSummary"]["convergenceSignal"], "diverged");
    }

    #[test]
    fn merge_convergence_signal_mixed() {
        // Spread between 0.2 and 0.4 -> "mixed"
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("a".into());
        b1.confidence = Some(0.5);
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("b".into());
        b2.confidence = Some(0.8); // spread = 0.3, between 0.2 and 0.4
        engine.process(b2).unwrap();

        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        let result = engine.process(merge).unwrap();

        assert_eq!(result["mergeSummary"]["convergenceSignal"], "mixed");
    }

    #[test]
    fn merge_convergence_insufficient_without_confidence() {
        // Branches without confidence set -> "insufficient"
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("a".into());
        // No confidence set
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("b".into());
        // No confidence set
        engine.process(b2).unwrap();

        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        let result = engine.process(merge).unwrap();

        assert_eq!(result["mergeSummary"]["convergenceSignal"], "insufficient");
    }

    #[test]
    fn merge_branch_outcome_without_done_reason() {
        // Branch that never set done_reason should have null/missing doneReason
        let mut engine = make_engine();
        engine.process(make_step(1, 10)).unwrap();

        let mut b1 = make_step(2, 10);
        b1.branch_from_step = Some(1);
        b1.branch_id = Some("a".into());
        b1.confidence = Some(0.6);
        // No done_reason
        engine.process(b1).unwrap();

        let mut b2 = make_step(3, 10);
        b2.branch_from_step = Some(1);
        b2.branch_id = Some("b".into());
        b2.confidence = Some(0.7);
        engine.process(b2).unwrap();

        let mut merge = make_step(4, 10);
        merge.continuation_mode = Some("merge".into());
        let result = engine.process(merge).unwrap();

        let outcomes = result["mergeSummary"]["branchOutcomes"].as_array().unwrap();
        for outcome in outcomes {
            // doneReason should be null (skipped in serialization) or not present
            assert!(outcome.get("doneReason").is_none() || outcome["doneReason"].is_null(),
                "doneReason should be absent when not set");
        }
    }
}
