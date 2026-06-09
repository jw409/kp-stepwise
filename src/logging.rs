use chrono::Utc;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

use crate::planner::StepData;

/// Persistent JSONL logger that appends step records to
/// `var/stepwise_logs/{session_id}.jsonl`.
pub struct PersistentLogger {
    session_id: String,
    log_file: Option<PathBuf>,
    project_path: String,
    model_id: String,
    client_type: String,
    profile_name: String,
}

impl PersistentLogger {
    pub fn new(model_id: &str, client_type: &str, profile_name: &str) -> Self {
        let session_id = Self::resolve_session_id();
        let project_path = Self::resolve_project_path();
        let log_dir = Self::resolve_log_dir(&project_path);
        let log_file = log_dir.map(|d| d.join(format!("{}.jsonl", session_id)));

        if let Some(ref lf) = log_file {
            tracing::info!(path = %lf.display(), "JSONL logging enabled");
        } else {
            tracing::warn!("could not create log directory, persistent logging disabled");
        }

        Self {
            session_id,
            log_file,
            project_path,
            model_id: model_id.to_string(),
            client_type: client_type.to_string(),
            profile_name: profile_name.to_string(),
        }
    }

    /// Append a step record to the JSONL log. Fire-and-forget on errors.
    pub fn persist(&self, step: &StepData) {
        let Some(ref log_file) = self.log_file else {
            return;
        };

        let record = json!({
            "timestamp": Utc::now().to_rfc3339(),
            "sessionId": self.session_id,
            "projectPath": self.project_path,
            "clientType": self.client_type,
            "modelId": self.model_id,
            "profile": self.profile_name,
            "step": step.step,
            "stepNumber": step.step_number,
            "totalSteps": step.total_steps,
            "nextStepNeeded": step.next_step_needed,
            "isRevision": step.is_revision,
            "revisesStep": step.revises_step,
            "branchFromStep": step.branch_from_step,
            "branchId": step.branch_id,
            "continuationMode": step.continuation_mode,
            "exploreCount": step.explore_count,
            "proposals": step.proposals,
            "layer": step.layer,
            "confidence": step.confidence,
            "doneReason": step.done_reason,
            "searchQuery": step.search_query,
        });

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

    fn resolve_session_id() -> String {
        std::env::var("CLAUDE_SESSION_ID")
            .or_else(|_| std::env::var("TALENTOS_SESSION_ID"))
            .unwrap_or_else(|_| {
                format!("st-{}-{}", Utc::now().timestamp_millis(), &uuid::Uuid::new_v4().to_string()[..8])
            })
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
    pub(crate) fn new_with_path(log_file: Option<PathBuf>, model_id: &str, client_type: &str, profile_name: &str) -> Self {
        Self {
            session_id: "test-session".into(),
            log_file,
            project_path: "/test".into(),
            model_id: model_id.into(),
            client_type: client_type.into(),
            profile_name: profile_name.into(),
        }
    }

    #[allow(dead_code)] // Available for diagnostics
    pub(crate) fn log_file_path(&self) -> Option<&Path> {
        self.log_file.as_deref()
    }

    fn resolve_log_dir(project_path: &str) -> Option<PathBuf> {
        let candidates = [
            PathBuf::from(project_path).join("var/stepwise_logs"),
            PathBuf::from(project_path).join("talent-os/var/stepwise_logs"),
        ];

        for dir in &candidates {
            if let Some(parent) = dir.parent() {
                if Path::new(parent).exists() {
                    if fs::create_dir_all(dir).is_ok() {
                        return Some(dir.clone());
                    }
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
        assert_eq!(record["stepNumber"], 1);
        assert_eq!(record["totalSteps"], 5);
        assert_eq!(record["confidence"], 0.7);
        assert!(record["timestamp"].is_string());
        assert!(record["sessionId"].is_string());
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
        std::env::remove_var("CLAUDE_SESSION_ID");
        std::env::remove_var("TALENTOS_SESSION_ID");

        let logger = PersistentLogger::new("test-model", "test-client", "Default");

        std::env::remove_var("TALENTOS_PROJECT_PATH");

        // Should have created a log file path
        assert!(logger.log_file_path().is_some());
        let path = logger.log_file_path().unwrap();
        assert!(path.to_str().unwrap().contains("stepwise_logs"));
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
