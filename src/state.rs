//! Server state management.
//!
//! Manages the state of the current runner, especially
//! the creation time for the deletion logic.
//!
//! The state is persisted to `config/state.json` to survive
//! program restarts.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Errors during state management.
#[derive(Error, Debug)]
pub enum StateError {
    #[error("Failed to read state file: {0}")]
    Read(#[from] std::io::Error),

    #[error("Failed to serialize state: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// State of an active runner.
///
/// NOTE: `triggered_by_project` and `triggered_by_pipeline` were removed -
/// this information is passed directly during CSV logging and doesn't need
/// to be stored in state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerState {
    /// Hetzner server ID
    pub server_id: u64,
    /// Server name
    pub server_name: String,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

impl RunnerState {
    /// Creates a new runner state with current timestamp.
    pub fn new(server_id: u64, server_name: String) -> Self {
        let created_at = Utc::now();
        info!(
            "Runner state created: Server {} (ID: {})",
            server_name, server_id
        );

        Self {
            server_id,
            server_name,
            created_at,
        }
    }

    // NOTE: `with_created_at()` was removed - state is now deserialized via JSON,
    // which automatically restores `created_at`.

    /// Calculates how long the server has been running (in minutes).
    pub fn uptime_minutes(&self) -> u64 {
        let duration = Utc::now().signed_duration_since(self.created_at);
        duration.num_minutes().max(0) as u64
    }

    /// Checks if the server has reached the minimum runtime.
    ///
    /// # Arguments
    /// * `min_minutes` - Minimum runtime in minutes
    pub fn has_min_uptime(&self, min_minutes: u32) -> bool {
        self.uptime_minutes() >= min_minutes as u64
    }

    /// Calculates minutes until the next billing cycle.
    ///
    /// Hetzner charges per started hour **from server creation**,
    /// not per clock hour!
    pub fn minutes_until_next_billing_cycle(&self) -> u64 {
        let uptime = self.uptime_minutes();
        let minutes_in_current_billing_hour = uptime % 60;

        if minutes_in_current_billing_hour == 0 && uptime > 0 {
            60 // Exactly at hour boundary = 60 minutes until next
        } else {
            60 - minutes_in_current_billing_hour
        }
    }

    /// Checks if it's a good time to delete.
    ///
    /// Ideal: 5 minutes before the next full hour,
    /// to optimally utilize the billing cycle.
    pub fn should_delete(&self, min_lifetime_minutes: u32, buffer_minutes: u64) -> bool {
        let uptime = self.uptime_minutes();
        let minutes_to_billing = self.minutes_until_next_billing_cycle();

        debug!(
            "Delete check: uptime={}min, min_lifetime={}min, until_billing={}min, buffer={}min",
            uptime, min_lifetime_minutes, minutes_to_billing, buffer_minutes
        );

        // Minimum runtime must be reached
        if uptime < min_lifetime_minutes as u64 {
            debug!("Minimum runtime not yet reached");
            return false;
        }

        // Ideally delete 5 minutes before the next full hour
        // OR if we've already run for a full hour+ and are close to the next one
        if minutes_to_billing <= buffer_minutes {
            info!(
                "Good time to delete: {} minutes until next billing cycle",
                minutes_to_billing
            );
            return true;
        }

        // If we're well over the minimum runtime (e.g., 50+ minutes),
        // and there are no active pipelines, also delete
        // (checked in main loop)

        debug!("Not yet the optimal delete time");
        false
    }

    /// Force-delete check: Minimum runtime reached, regardless of billing cycle.
    ///
    /// Used when no pipelines are active anymore and we don't want
    /// to wait for the optimal time.
    pub fn can_force_delete(&self, min_lifetime_minutes: u32) -> bool {
        self.has_min_uptime(min_lifetime_minutes)
    }
}

/// Persisted state (saved as JSON).
#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    runner: Option<RunnerState>,
}

/// Orchestrator state - manages the entire application state.
#[derive(Debug, Default)]
pub struct OrchestratorState {
    /// Current runner (if present)
    pub runner: Option<RunnerState>,
    /// Path to the state file
    state_file_path: Option<std::path::PathBuf>,
}

impl OrchestratorState {
    /// Creates a new orchestrator state without persistence (for tests only).
    #[cfg(test)]
    pub fn new() -> Self {
        Self {
            runner: None,
            state_file_path: None,
        }
    }

    /// Creates a new orchestrator state with persistence.
    ///
    /// Automatically loads the saved state if present.
    pub fn with_persistence<P: AsRef<Path>>(state_file: P) -> Result<Self, StateError> {
        let path = state_file.as_ref().to_path_buf();
        let mut state = Self {
            runner: None,
            state_file_path: Some(path.clone()),
        };

        // Try to load state
        if path.exists() {
            match state.load_from_file(&path) {
                Ok(()) => {
                    if state.runner.is_some() {
                        info!("State loaded from file: {}", path.display());
                    }
                }
                Err(e) => {
                    warn!("Could not load state (ignoring): {}", e);
                }
            }
        }

        Ok(state)
    }

    /// Loads the state from a file.
    fn load_from_file(&mut self, path: &Path) -> Result<(), StateError> {
        let content = std::fs::read_to_string(path)?;
        let persisted: PersistedState = serde_json::from_str(&content)?;
        self.runner = persisted.runner;
        Ok(())
    }

    /// Saves the state to file.
    fn save_to_file(&self) -> Result<(), StateError> {
        if let Some(ref path) = self.state_file_path {
            let persisted = PersistedState {
                runner: self.runner.clone(),
            };
            let content = serde_json::to_string_pretty(&persisted)?;
            std::fs::write(path, content)?;
            debug!("State saved: {}", path.display());
        }
        Ok(())
    }

    /// Sets the active runner and saves the state.
    pub fn set_runner(&mut self, state: RunnerState) {
        info!(
            "Active runner set: {} (ID: {})",
            state.server_name, state.server_id
        );
        self.runner = Some(state);

        if let Err(e) = self.save_to_file() {
            warn!("Error saving state: {}", e);
        }
    }

    /// Removes the active runner and saves the state.
    pub fn clear_runner(&mut self) {
        if let Some(ref runner) = self.runner {
            info!(
                "Runner removed: {} (runtime: {} minutes)",
                runner.server_name,
                runner.uptime_minutes()
            );
        }
        self.runner = None;

        if let Err(e) = self.save_to_file() {
            warn!("Error saving state: {}", e);
        }
    }

    /// Checks if a runner is active.
    pub fn has_runner(&self) -> bool {
        self.runner.is_some()
    }

    /// Returns the uptime of the current runner (if present).
    pub fn runner_uptime(&self) -> Option<u64> {
        self.runner.as_ref().map(|r| r.uptime_minutes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runner_state_creation() {
        let state = RunnerState::new(12345, "test-runner".to_string());

        assert_eq!(state.server_id, 12345);
        assert_eq!(state.server_name, "test-runner");
        assert!(state.uptime_minutes() < 1);
    }

    #[test]
    fn test_orchestrator_state() {
        let mut state = OrchestratorState::new();
        assert!(!state.has_runner());

        let runner = RunnerState::new(12345, "test-runner".to_string());
        state.set_runner(runner);

        assert!(state.has_runner());

        state.clear_runner();
        assert!(!state.has_runner());
    }

    #[test]
    fn test_state_serialization() {
        let runner = RunnerState::new(12345, "test-runner".to_string());
        let json = serde_json::to_string(&runner).unwrap();
        let restored: RunnerState = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.server_id, 12345);
        assert_eq!(restored.server_name, "test-runner");
    }
}
