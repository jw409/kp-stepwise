use chrono::Utc;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

use crate::planner::StepData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogMode {
    Off,
    Metadata,
    Full,
}

impl LogMode {
    fn from_env() -> Self {
        if let Ok(mode) = std::env::var("KP_STEPWISE_LOG_MODE") {
            return Self::parse(&mode);
        }

        if std::env::var("DISABLE_STEP_LOGGING")
            .or_else(|_| std::env::var("DISABLE_THOUGHT_LOGGING"))
            .is_ok_and(|v| v.eq_ignore_ascii_case("true"))
        {
            return Self::Off;
        }

        Self::Full
    }

    fn parse(mode: &str) -> Self {
        match mode.to_lowercase().as_str() {
            "off" => Self::Off,
            "metadata" => Self::Metadata,
            "full" => Self::Full,
            other => {
                tracing::warn!(mode = other, "invalid KP_STEPWISE_LOG_MODE; using full");
                Self::Full
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Metadata => "metadata",
            Self::Full => "full",
        }
    }
}

/// Persistent JSONL logger that appends channel-scoped step records to
/// `var/stepwise_logs/{room_id}/channels/{channel_id}.jsonl`.
pub struct PersistentLogger {
    session_id: String,
    room_id: String,
    channel_id: String,
    log_file: Option<PathBuf>,
    project_path: String,
    model_id: String,
    client_type: String,
    profile_name: String,
    log_mode: LogMode,
}

impl PersistentLogger {
    #[allow(dead_code)] // Retained for single-channel embedders and unit tests.
    pub fn new(model_id: &str, client_type: &str, profile_name: &str) -> Self {
        let session_id = Self::resolve_session_id();
        Self::new_scoped(
            model_id,
            client_type,
            profile_name,
            &session_id,
            &session_id,
            "main",
        )
    }

    pub fn new_scoped(
        model_id: &str,
        client_type: &str,
        profile_name: &str,
        session_id: &str,
        room_id: &str,
        channel_id: &str,
    ) -> Self {
        let project_path = Self::resolve_project_path();
        let requested_log_mode = LogMode::from_env();
        let log_root = (requested_log_mode != LogMode::Off)
            .then(|| Self::resolve_log_dir(&project_path))
            .flatten();
        let log_file = log_root.and_then(|root| {
            let channel_dir = root.join(room_id).join("channels");
            fs::create_dir_all(&channel_dir)
                .ok()
                .map(|_| channel_dir.join(format!("{channel_id}.jsonl")))
        });
        let log_mode = if requested_log_mode != LogMode::Off && log_file.is_none() {
            LogMode::Off
        } else {
            requested_log_mode
        };

        if requested_log_mode == LogMode::Off {
            tracing::info!("persistent JSONL logging disabled");
        } else if let Some(ref lf) = log_file {
            tracing::info!(path = %lf.display(), "JSONL logging enabled");
        } else {
            tracing::warn!("could not create log directory, persistent logging disabled");
        }

        Self {
            session_id: session_id.to_string(),
            room_id: room_id.to_string(),
            channel_id: channel_id.to_string(),
            log_file,
            project_path,
            model_id: model_id.to_string(),
            client_type: client_type.to_string(),
            profile_name: profile_name.to_string(),
            log_mode,
        }
    }

    /// Append a step record to the JSONL log. Fire-and-forget on errors.
    pub fn persist(&self, step: &StepData) {
        let Some(ref log_file) = self.log_file else {
            return;
        };

        let mut record = json!({
            "schemaVersion": 2,
            "recordType": "workflow_checkpoint",
            "eventId": uuid::Uuid::new_v4().to_string(),
            "timestamp": Utc::now().to_rfc3339(),
            "sessionId": self.session_id,
            "roomId": self.room_id,
            "channelId": self.channel_id,
            "projectPath": self.project_path,
            "clientType": self.client_type,
            "modelId": self.model_id,
            "profile": self.profile_name,
            "stepNumber": step.step_number,
            "totalSteps": step.total_steps,
            "nextStepNeeded": step.next_step_needed,
            "isRevision": step.is_revision,
            "revisesStep": step.revises_step,
            "branchFromStep": step.branch_from_step,
            "branchId": step.branch_id,
            "turnId": step.turn_id,
            "checkpointKind": step.checkpoint_kind,
            "continuationMode": step.continuation_mode,
            "exploreCount": step.explore_count,
            "layer": step.layer,
            "confidence": step.confidence,
            "doneReason": step.done_reason,
            "hasEvidence": step.evidence.as_ref().is_some_and(|items| !items.is_empty()),
            "hasOpenQuestions": step.open_questions.as_ref().is_some_and(|items| !items.is_empty()),
            "hasNextAction": step.next_action.is_some(),
        });

        if self.log_mode == LogMode::Full {
            // `step` is retained for existing local analytics. `checkpoint`
            // states the field's intended semantics for new consumers.
            record["step"] = json!(step.step);
            record["checkpoint"] = json!(step.step);
            record["evidence"] = json!(step.evidence);
            record["openQuestions"] = json!(step.open_questions);
            record["nextAction"] = json!(step.next_action);
            record["proposals"] = json!(step.proposals);
            record["searchQuery"] = json!(step.search_query);
        }

        let line = match serde_json::to_string(&record) {
            Ok(s) => s + "\n",
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize step record");
                return;
            }
        };

        if let Err(e) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(line.as_bytes())
            })
        {
            // Fire and forget
            tracing::warn!(error = %e, "log write failed");
        }
    }

    pub(crate) fn resolve_session_id() -> String {
        let raw = std::env::var("STEPWISE_SESSION_ID")
            .or_else(|_| std::env::var("CODEX_THREAD_ID"))
            .or_else(|_| std::env::var("CLAUDE_SESSION_ID"))
            .or_else(|_| std::env::var("TALENTOS_SESSION_ID"))
            .unwrap_or_else(|_| {
                format!(
                    "st-{}-{}",
                    Utc::now().timestamp_millis(),
                    &uuid::Uuid::new_v4().to_string()[..8]
                )
            });

        let sanitized: String = raw
            .chars()
            .take(128)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
            format!("st-{}", &uuid::Uuid::new_v4().to_string()[..8])
        } else {
            sanitized
        }
    }

    fn resolve_project_path() -> String {
        std::env::var("TALENTOS_PROJECT_PATH")
            .or_else(|_| std::env::var("PROJECT_ROOT"))
            .unwrap_or_else(|_| {
                std::env::current_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| ".".to_string())
            })
    }

    /// Test-friendly constructor that takes an explicit log file path.
    #[cfg(test)]
    pub(crate) fn new_with_path(
        log_file: Option<PathBuf>,
        model_id: &str,
        client_type: &str,
        profile_name: &str,
    ) -> Self {
        Self {
            session_id: "test-session".into(),
            room_id: "test-room".into(),
            channel_id: "test-channel".into(),
            log_file,
            project_path: "/test".into(),
            model_id: model_id.into(),
            client_type: client_type.into(),
            profile_name: profile_name.into(),
            log_mode: LogMode::Full,
        }
    }

    #[allow(dead_code)] // Available for diagnostics
    pub(crate) fn log_file_path(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn room_id(&self) -> &str {
        &self.room_id
    }

    pub(crate) fn channel_id(&self) -> &str {
        &self.channel_id
    }

    pub(crate) fn log_mode(&self) -> &str {
        self.log_mode.as_str()
    }

    fn resolve_log_dir(project_path: &str) -> Option<PathBuf> {
        let candidates = [
            PathBuf::from(project_path).join("var/stepwise_logs"),
            PathBuf::from(project_path).join("talent-os/var/stepwise_logs"),
        ];

        for dir in &candidates {
            if let Some(parent) = dir.parent() {
                if Path::new(parent).exists() && fs::create_dir_all(dir).is_ok() {
                    return Some(dir.clone());
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    fn make_test_step(num: u32) -> StepData {
        StepData {
            step: format!("Test step {}", num),
            step_number: num,
            total_steps: 5,
            next_step_needed: true,
            turn_id: Some(format!("turn-{}", num)),
            checkpoint_kind: Some("observation".into()),
            evidence: Some(vec!["unit test".into()]),
            open_questions: None,
            next_action: Some("continue".into()),
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
            confidence: Some(0.7),
            done_reason: None,
            context_window: None,
            search_context: None,
            search_query: None,
            incorporate_search: None,
        }
    }

    /// Use new_with_path to avoid env var races in parallel test execution.
    fn make_logger_in_tmp(tmp: &tempfile::TempDir) -> PersistentLogger {
        let log_file = tmp.path().join("test.jsonl");
        PersistentLogger::new_with_path(Some(log_file), "test-model", "test-client", "Default")
    }

    #[test]
    fn logger_with_path_has_log_file() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = make_logger_in_tmp(&tmp);
        assert!(logger.log_file_path().is_some());
    }

    #[test]
    fn persist_writes_valid_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = make_logger_in_tmp(&tmp);
        let step = make_test_step(1);
        logger.persist(&step);

        let log_path = logger.log_file_path().unwrap();
        assert!(log_path.exists(), "log file should exist after persist");

        let file = fs::File::open(log_path).unwrap();
        let reader = std::io::BufReader::new(file);
        let lines: Vec<String> = reader.lines().map(|l| l.unwrap()).collect();
        assert_eq!(lines.len(), 1, "should have exactly 1 line");

        let record: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(record["step"], "Test step 1");
        assert_eq!(record["checkpoint"], "Test step 1");
        assert_eq!(record["recordType"], "workflow_checkpoint");
        assert_eq!(record["schemaVersion"], 2);
        assert_eq!(record["turnId"], "turn-1");
        assert_eq!(record["checkpointKind"], "observation");
        assert_eq!(record["roomId"], "test-room");
        assert_eq!(record["channelId"], "test-channel");
        assert_eq!(record["stepNumber"], 1);
        assert_eq!(record["totalSteps"], 5);
        assert_eq!(record["confidence"], 0.7);
        assert!(record["timestamp"].is_string());
        assert!(record["sessionId"].is_string());
    }

    #[test]
    fn metadata_mode_omits_workflow_content_but_keeps_audit_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let mut logger = make_logger_in_tmp(&tmp);
        logger.log_mode = LogMode::Metadata;
        logger.persist(&make_test_step(1));

        let content = fs::read_to_string(logger.log_file_path().unwrap()).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert!(record.get("step").is_none());
        assert!(record.get("checkpoint").is_none());
        assert!(record.get("evidence").is_none());
        assert_eq!(record["hasEvidence"], true);
        assert_eq!(record["hasNextAction"], true);
        assert_eq!(record["recordType"], "workflow_checkpoint");
    }

    #[test]
    fn session_id_is_safe_for_use_as_a_filename() {
        std::env::set_var("STEPWISE_SESSION_ID", "../../turn / one");
        let session_id = PersistentLogger::resolve_session_id();
        std::env::remove_var("STEPWISE_SESSION_ID");

        assert_eq!(session_id, ".._.._turn___one");
        assert!(!session_id.contains('/'));
    }

    #[test]
    fn persist_appends_multiple_steps() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = make_logger_in_tmp(&tmp);
        logger.persist(&make_test_step(1));
        logger.persist(&make_test_step(2));
        logger.persist(&make_test_step(3));

        let log_path = logger.log_file_path().unwrap();
        let content = fs::read_to_string(log_path).unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, 3, "should have 3 lines after 3 persists");

        // Each line should be valid JSON
        for line in content.lines() {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
            assert!(parsed.is_ok(), "each line should be valid JSON");
        }
    }

    #[test]
    fn logger_none_path_has_no_log_file() {
        let logger = PersistentLogger::new_with_path(None, "test-model", "test-client", "Default");
        assert!(logger.log_file_path().is_none());
    }

    #[test]
    fn persist_noop_when_no_log_file() {
        let logger = PersistentLogger::new_with_path(None, "test-model", "test-client", "Default");
        // Should not panic
        logger.persist(&make_test_step(1));
    }

    #[test]
    fn resolve_log_dir_with_valid_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let var_dir = tmp.path().join("var");
        fs::create_dir(&var_dir).unwrap();

        let result = PersistentLogger::resolve_log_dir(tmp.path().to_str().unwrap());
        assert!(result.is_some());
        let dir = result.unwrap();
        assert!(dir.to_str().unwrap().contains("stepwise_logs"));
        assert!(dir.exists());
    }

    #[test]
    fn resolve_log_dir_nonexistent_parent() {
        let result = PersistentLogger::resolve_log_dir("/nonexistent/path/that/does/not/exist");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_log_dir_creates_subdir_when_talent_os_var_exists() {
        // Test the talent-os/var/stepwise_logs candidate path
        let tmp = tempfile::tempdir().unwrap();
        let talent_os_var = tmp.path().join("talent-os").join("var");
        fs::create_dir_all(&talent_os_var).unwrap();

        let result = PersistentLogger::resolve_log_dir(tmp.path().to_str().unwrap());
        assert!(result.is_some());
        let dir = result.unwrap();
        assert!(dir.to_str().unwrap().contains("stepwise_logs"));
    }

    #[test]
    fn new_logger_without_valid_project_path() {
        // When project path has no writable var/ directory, log_file should be None
        // This exercises line 27 (the else branch) and returns None from resolve_log_dir
        let logger = PersistentLogger::new_with_path(None, "m", "c", "p");
        assert!(logger.log_file_path().is_none());
        // Persisting should be a no-op (line 44-46)
        logger.persist(&make_test_step(1));
    }

    #[test]
    fn persist_handles_write_error_gracefully() {
        // Point to a file inside a nonexistent directory — write will fail
        let impossible_path = PathBuf::from("/nonexistent_dir_xyz/impossible.jsonl");
        let logger = PersistentLogger::new_with_path(Some(impossible_path), "m", "c", "p");
        // Should not panic — fire and forget (exercises lines 88-90)
        logger.persist(&make_test_step(1));
    }

    #[test]
    fn new_constructor_with_valid_project_path() {
        // Exercise the real new() constructor with a valid project path that has var/
        let tmp = tempfile::tempdir().unwrap();
        let var_dir = tmp.path().join("var");
        fs::create_dir(&var_dir).unwrap();

        // Set env vars to control the constructor behavior
        std::env::set_var("TALENTOS_PROJECT_PATH", tmp.path().to_str().unwrap());
        std::env::set_var("KP_STEPWISE_LOG_MODE", "full");
        std::env::remove_var("CLAUDE_SESSION_ID");
        std::env::remove_var("TALENTOS_SESSION_ID");

        let logger = PersistentLogger::new("test-model", "test-client", "Default");

        std::env::remove_var("TALENTOS_PROJECT_PATH");
        std::env::remove_var("KP_STEPWISE_LOG_MODE");

        // Should have created a log file path
        assert!(logger.log_file_path().is_some());
        let path = logger.log_file_path().unwrap();
        assert!(path.to_str().unwrap().contains("stepwise_logs"));
        assert!(path.to_str().unwrap().contains("channels"));
        assert!(path.ends_with("main.jsonl"));
    }

    #[test]
    fn scoped_logger_uses_room_and_channel_directories() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("var")).unwrap();
        std::env::set_var("TALENTOS_PROJECT_PATH", tmp.path().to_str().unwrap());
        std::env::set_var("KP_STEPWISE_LOG_MODE", "full");

        let logger = PersistentLogger::new_scoped(
            "test-model",
            "test-client",
            "Default",
            "host-session",
            "repair-eval",
            "opus-gold-type",
        );

        std::env::remove_var("TALENTOS_PROJECT_PATH");
        std::env::remove_var("KP_STEPWISE_LOG_MODE");

        assert_eq!(logger.session_id(), "host-session");
        assert_eq!(logger.room_id(), "repair-eval");
        assert_eq!(logger.channel_id(), "opus-gold-type");
        assert!(logger
            .log_file_path()
            .unwrap()
            .ends_with("repair-eval/channels/opus-gold-type.jsonl"));
    }

    #[test]
    fn persist_records_all_step_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let logger = make_logger_in_tmp(&tmp);

        let mut step = make_test_step(2);
        step.is_revision = Some(true);
        step.revises_step = Some(1);
        step.branch_from_step = Some(1);
        step.branch_id = Some("test-branch".into());
        step.continuation_mode = Some("explore".into());
        step.explore_count = Some(3);
        step.proposals = Some(vec!["A".into(), "B".into()]);
        step.layer = Some(2);
        step.done_reason = Some("sufficient".into());
        step.search_query = Some("test query".into());

        logger.persist(&step);

        let log_path = logger.log_file_path().unwrap();
        let content = fs::read_to_string(log_path).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

        assert_eq!(record["isRevision"], true);
        assert_eq!(record["revisesStep"], 1);
        assert_eq!(record["branchFromStep"], 1);
        assert_eq!(record["branchId"], "test-branch");
        assert_eq!(record["continuationMode"], "explore");
        assert_eq!(record["exploreCount"], 3);
        assert_eq!(record["layer"], 2);
        assert_eq!(record["doneReason"], "sufficient");
        assert_eq!(record["searchQuery"], "test query");
        assert_eq!(record["proposals"].as_array().unwrap().len(), 2);
    }
}
