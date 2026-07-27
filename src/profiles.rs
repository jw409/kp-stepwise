use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-model tuning profile for stepwise planning behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuningProfile {
    pub model_pattern: String,
    pub display_name: String,
    pub default_explore_count: u32,
    pub max_explore_count: u32,
    pub default_layer_depth: u32,
    pub branching_threshold: f64,
    pub confidence_threshold: f64,
    pub context_window: String,
    pub token_budget_multiplier: f64,
    pub guidance: String,
}

/// Built-in profiles for common model families.
pub fn default_profiles() -> Vec<TuningProfile> {
    vec![
        // GPT-5.6 Sol already performs native reasoning. Use this MCP as a
        // sparse external checkpoint ledger rather than a narrated scratchpad.
        TuningProfile {
            model_pattern: "gpt-5\\.6(-sol)?|(^|[/_-])sol($|[/_-])".into(),
            display_name: "OpenAI Sol".into(),
            default_explore_count: 2,
            max_explore_count: 3,
            default_layer_depth: 2,
            branching_threshold: 0.4,
            confidence_threshold: 0.85,
            context_window: "compact".into(),
            token_budget_multiplier: 1.0,
            guidance: "Use sparse checkpoints at decisions, evidence changes, revisions, and handoffs; rely on native reasoning between calls.".into(),
        },
        // Other OpenAI reasoning and coding models.
        TuningProfile {
            model_pattern: "(^|[/_-])(gpt|o[1-9]|codex)([-/_.]|$)|openai".into(),
            display_name: "OpenAI Reasoning".into(),
            default_explore_count: 2,
            max_explore_count: 4,
            default_layer_depth: 2,
            branching_threshold: 0.45,
            confidence_threshold: 0.8,
            context_window: "compact".into(),
            token_budget_multiplier: 1.0,
            guidance: "Keep checkpoints concise and outcome-focused; add one at a decision, revision, evidence update, or handoff.".into(),
        },
        // Gemini Flash: wide/fast, many alternatives, quick convergence
        TuningProfile {
            model_pattern: "gemini.*flash".into(),
            display_name: "Gemini Flash".into(),
            default_explore_count: 5,
            max_explore_count: 7,
            default_layer_depth: 2,
            branching_threshold: 0.6,
            confidence_threshold: 0.75,
            context_window: "normal".into(),
            token_budget_multiplier: 1.3,
            guidance: "Wide exploration optimal. Generate 5-7 alternatives at each layer. Branch liberally. Quick parallel evaluation, then converge.".into(),
        },
        // Gemini Pro/Ultra: has native extended thinking — lean on depth, fewer branches
        TuningProfile {
            model_pattern: "gemini.*(pro|ultra|exp|thinking)".into(),
            display_name: "Gemini Pro".into(),
            default_explore_count: 3,
            max_explore_count: 4,
            default_layer_depth: 3,
            branching_threshold: 0.5,
            confidence_threshold: 0.85,
            context_window: "expanded".into(),
            token_budget_multiplier: 1.5,
            guidance: "Deep thinking model. Fewer branches, deeper analysis per branch. Use layer=3 for implementation detail. High confidence threshold — this model's reasoning is strong, trust it. Prefer depth over breadth.".into(),
        },
        // Gemini catch-all (new models, nano, etc.)
        TuningProfile {
            model_pattern: "gemini".into(),
            display_name: "Gemini".into(),
            default_explore_count: 4,
            max_explore_count: 5,
            default_layer_depth: 2,
            branching_threshold: 0.55,
            confidence_threshold: 0.8,
            context_window: "normal".into(),
            token_budget_multiplier: 1.3,
            guidance: "Balanced Gemini model. 4-5 alternatives, moderate depth. Branch when uncertain.".into(),
        },
        TuningProfile {
            model_pattern: "deepseek".into(),
            display_name: "DeepSeek".into(),
            default_explore_count: 3,
            max_explore_count: 5,
            default_layer_depth: 3,
            branching_threshold: 0.5,
            confidence_threshold: 0.8,
            context_window: "expanded".into(),
            token_budget_multiplier: 1.3,
            guidance: "Deep hierarchical exploration optimal. Use 3 layers of abstraction. Go deep on each alternative. Prefer depth over breadth.".into(),
        },
        TuningProfile {
            model_pattern: "grok".into(),
            display_name: "Grok".into(),
            default_explore_count: 4,
            max_explore_count: 6,
            default_layer_depth: 2,
            branching_threshold: 0.55,
            confidence_threshold: 0.7,
            context_window: "normal".into(),
            token_budget_multiplier: 1.3,
            guidance: "Creative exploration optimal. Generate unconventional alternatives. Challenge assumptions. Look for non-obvious solutions.".into(),
        },
        TuningProfile {
            model_pattern: "claude".into(),
            display_name: "Claude".into(),
            default_explore_count: 4,
            max_explore_count: 5,
            default_layer_depth: 2,
            branching_threshold: 0.6,
            confidence_threshold: 0.75,
            context_window: "normal".into(),
            token_budget_multiplier: 1.3,
            guidance: "Balanced exploration optimal. 3-5 alternatives per layer. Synthesize insights across branches. Strong at meta-analysis and finding connections between ideas.".into(),
        },
        TuningProfile {
            model_pattern: "llama|nemotron".into(),
            display_name: "Llama/Nemotron".into(),
            default_explore_count: 4,
            max_explore_count: 6,
            default_layer_depth: 2,
            branching_threshold: 0.55,
            confidence_threshold: 0.75,
            context_window: "normal".into(),
            token_budget_multiplier: 1.3,
            guidance: "Powerful general exploration. Good at systematic analysis. 4-6 alternatives recommended. Strong at technical depth.".into(),
        },
    ]
}

/// Fallback profile for unrecognized models.
pub fn fallback_profile() -> TuningProfile {
    TuningProfile {
        model_pattern: ".*".into(),
        display_name: "Default".into(),
        default_explore_count: 3,
        max_explore_count: 5,
        default_layer_depth: 2,
        branching_threshold: 0.6,
        confidence_threshold: 0.75,
        context_window: "normal".into(),
        token_budget_multiplier: 1.3,
        guidance: "Balanced exploration. Generate 3-5 alternatives at each decision point. Branch when uncertain. Exit when confident.".into(),
    }
}

/// Try loading profiles from a JSON file, falling back to defaults.
pub fn load_profiles() -> Vec<TuningProfile> {
    let candidates: Vec<Option<String>> = vec![
        std::env::var("STEPWISE_PROFILES").ok(),
        std::env::var("SEQUENTIAL_THINKING_PROFILES").ok(), // legacy name
        Some("talent-os/etc/stepwise_profiles.json".into()),
        Some("etc/stepwise_profiles.json".into()),
        // Legacy file locations
        Some("talent-os/etc/sequential_thinking_profiles.json".into()),
        Some("etc/sequential_thinking_profiles.json".into()),
    ];

    for candidate in candidates.into_iter().flatten() {
        let path = Path::new(&candidate);
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str::<Vec<TuningProfile>>(&content) {
                    Ok(profiles) => {
                        tracing::info!(
                            count = profiles.len(),
                            path = %path.display(),
                            "loaded tuning profiles from file"
                        );
                        return profiles;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "failed to parse profiles");
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "failed to read profiles");
                }
            }
        }
    }

    tracing::info!("using default tuning profiles");
    default_profiles()
}

/// Match a model ID against available profiles.
#[allow(clippy::needless_pass_by_value)]
pub fn get_profile_for_model(model_id: &str, profiles: &[TuningProfile]) -> TuningProfile {
    let normalized = model_id.to_lowercase();

    for profile in profiles {
        if let Ok(re) = Regex::new(&format!("(?i){}", profile.model_pattern)) {
            if re.is_match(&normalized) {
                tracing::info!(
                    profile = %profile.display_name,
                    model = model_id,
                    "matched tuning profile"
                );
                return profile.clone();
            }
        }
    }

    tracing::info!(model = model_id, "no profile match, using fallback");
    fallback_profile()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profiles_has_at_least_seven() {
        let profiles = default_profiles();
        assert!(
            profiles.len() >= 9,
            "expected at least 9 profiles, got {}",
            profiles.len()
        );
    }

    #[test]
    fn sol_model_matches_sol_profile() {
        let profiles = default_profiles();
        for model in ["gpt-5.6-sol", "gpt-5.6", "openai/gpt-5.6-sol", "sol"] {
            let p = get_profile_for_model(model, &profiles);
            assert_eq!(p.display_name, "OpenAI Sol", "model {model}");
        }
    }

    #[test]
    fn other_openai_models_match_reasoning_profile() {
        let profiles = default_profiles();
        for model in ["gpt-5.3-codex", "openai/o3", "o1-pro"] {
            let p = get_profile_for_model(model, &profiles);
            assert_eq!(p.display_name, "OpenAI Reasoning", "model {model}");
        }
    }

    #[test]
    fn claude_model_matches_claude_profile() {
        let profiles = default_profiles();
        let p = get_profile_for_model("claude-3-opus", &profiles);
        assert_eq!(p.display_name, "Claude");
    }

    #[test]
    fn claude_case_insensitive() {
        let profiles = default_profiles();
        let p = get_profile_for_model("Claude-Sonnet-4", &profiles);
        assert_eq!(p.display_name, "Claude");
    }

    #[test]
    fn deepseek_model_matches() {
        let profiles = default_profiles();
        let p = get_profile_for_model("deepseek-v3", &profiles);
        assert_eq!(p.display_name, "DeepSeek");
    }

    #[test]
    fn gemini_flash_matches() {
        let profiles = default_profiles();
        let p = get_profile_for_model("gemini-2.0-flash-exp", &profiles);
        assert_eq!(p.display_name, "Gemini Flash");
    }

    #[test]
    fn gemini_pro_matches() {
        let profiles = default_profiles();
        let p = get_profile_for_model("gemini-2.5-pro", &profiles);
        assert_eq!(p.display_name, "Gemini Pro");
    }

    #[test]
    fn gemini_pro_thinking_matches() {
        let profiles = default_profiles();
        let p = get_profile_for_model("gemini-2.5-pro-thinking", &profiles);
        assert_eq!(p.display_name, "Gemini Pro");
    }

    #[test]
    fn gemini_ultra_matches() {
        let profiles = default_profiles();
        let p = get_profile_for_model("gemini-ultra", &profiles);
        assert_eq!(p.display_name, "Gemini Pro");
    }

    #[test]
    fn gemini_nano_falls_to_catchall() {
        let profiles = default_profiles();
        let p = get_profile_for_model("gemini-nano", &profiles);
        assert_eq!(p.display_name, "Gemini");
    }

    #[test]
    fn grok_matches() {
        let profiles = default_profiles();
        let p = get_profile_for_model("grok-2", &profiles);
        assert_eq!(p.display_name, "Grok");
    }

    #[test]
    fn nemotron_matches_llama_profile() {
        let profiles = default_profiles();
        let p = get_profile_for_model("nemotron-49b", &profiles);
        assert_eq!(p.display_name, "Llama/Nemotron");
    }

    #[test]
    fn llama_matches_llama_profile() {
        let profiles = default_profiles();
        let p = get_profile_for_model("llama-3.1-70b", &profiles);
        assert_eq!(p.display_name, "Llama/Nemotron");
    }

    #[test]
    fn unknown_model_gets_fallback() {
        let profiles = default_profiles();
        let p = get_profile_for_model("totally-unknown-model-xyz", &profiles);
        assert_eq!(p.display_name, "Default");
    }

    #[test]
    fn fallback_profile_has_reasonable_values() {
        let p = fallback_profile();
        assert_eq!(p.display_name, "Default");
        assert!(p.branching_threshold > 0.0 && p.branching_threshold < 1.0);
        assert!(p.confidence_threshold > 0.0 && p.confidence_threshold < 1.0);
        assert!(p.default_explore_count >= 1);
        assert!(p.max_explore_count >= p.default_explore_count);
        assert!(p.token_budget_multiplier >= 1.0);
    }

    #[test]
    fn all_profiles_have_valid_regex_patterns() {
        let profiles = default_profiles();
        for profile in &profiles {
            let re = regex::Regex::new(&format!("(?i){}", profile.model_pattern));
            assert!(
                re.is_ok(),
                "invalid regex in profile {}: {}",
                profile.display_name,
                profile.model_pattern
            );
        }
    }

    #[test]
    fn profile_thresholds_ordering() {
        // branching_threshold should be <= confidence_threshold for all profiles
        let profiles = default_profiles();
        for p in &profiles {
            assert!(
                p.branching_threshold <= p.confidence_threshold,
                "{}: branching {} > confidence {}",
                p.display_name,
                p.branching_threshold,
                p.confidence_threshold
            );
        }
    }

    // ---- load_profiles with actual files ----
    // All env-var-dependent tests in one function to avoid parallel races.

    #[test]
    fn load_profiles_file_scenarios() {
        // 1. Valid custom file via env var
        let tmp = tempfile::tempdir().unwrap();
        let profiles_file = tmp.path().join("custom_profiles.json");
        let custom = vec![TuningProfile {
            model_pattern: "custom-model".into(),
            display_name: "CustomTest".into(),
            default_explore_count: 2,
            max_explore_count: 3,
            default_layer_depth: 1,
            branching_threshold: 0.5,
            confidence_threshold: 0.8,
            context_window: "compact".into(),
            token_budget_multiplier: 1.0,
            guidance: "Custom guidance.".into(),
        }];
        std::fs::write(&profiles_file, serde_json::to_string(&custom).unwrap()).unwrap();

        std::env::set_var("STEPWISE_PROFILES", profiles_file.to_str().unwrap());
        let loaded = load_profiles();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].display_name, "CustomTest");

        // 2. Invalid JSON falls through to defaults
        let bad_file = tmp.path().join("bad_profiles.json");
        std::fs::write(&bad_file, "not valid json!!!").unwrap();
        std::env::set_var("STEPWISE_PROFILES", bad_file.to_str().unwrap());
        let loaded = load_profiles();
        assert!(
            loaded.len() >= 5,
            "invalid JSON should fall back to defaults"
        );

        // 3. Empty array is valid JSON — returns 0 profiles
        let empty_file = tmp.path().join("empty_profiles.json");
        std::fs::write(&empty_file, "[]").unwrap();
        std::env::set_var("STEPWISE_PROFILES", empty_file.to_str().unwrap());
        let loaded = load_profiles();
        assert_eq!(loaded.len(), 0);

        // 4. Unreadable file (directory) falls through to defaults
        let dir_path = tmp.path().join("a_directory");
        std::fs::create_dir(&dir_path).unwrap();
        std::env::set_var("STEPWISE_PROFILES", dir_path.to_str().unwrap());
        let loaded = load_profiles();
        assert!(
            loaded.len() >= 5,
            "unreadable file should fall back to defaults"
        );

        // 5. No env var — defaults
        std::env::remove_var("STEPWISE_PROFILES");
        let loaded = load_profiles();
        assert!(loaded.len() >= 5, "no env var should return defaults");
    }
}
