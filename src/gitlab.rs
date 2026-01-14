//! GitLab API client for pipeline status queries.
//!
//! Uses raw HTTP requests to query all projects and their pipeline status.

use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::config::GitLabConfig;

/// Errors that can occur during GitLab API calls.
#[derive(Error, Debug)]
pub enum GitLabError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("GitLab API error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("Invalid API response: {0}")]
    Parse(String),
}

/// A GitLab project (simplified representation).
///
/// NOTE: Additional fields like `name`, `description` etc. were removed -
/// not needed in this context, only `id` and `path_with_namespace` are relevant.
#[derive(Debug, Deserialize, Clone)]
pub struct Project {
    /// Project ID
    pub id: u64,
    /// Full path (e.g., "group/project")
    pub path_with_namespace: String,
}

/// A GitLab pipeline.
///
/// NOTE: Additional fields like `ref`, `web_url`, `created_at` etc. were removed -
/// not needed in this context, only `id` and `status` are relevant.
#[derive(Debug, Deserialize, Clone)]
pub struct Pipeline {
    /// Pipeline ID
    pub id: u64,
    /// Status (pending, running, success, failed, etc.)
    pub status: String,
}

/// Information about an active pipeline with project context.
#[derive(Debug, Clone)]
pub struct ActivePipeline {
    /// The project the pipeline belongs to
    pub project: Project,
    /// The pipeline itself
    pub pipeline: Pipeline,
}

/// GitLab API client.
pub struct GitLabClient {
    /// HTTP client
    client: Client,
    /// Base URL of the GitLab instance
    base_url: String,
    /// API token
    token: String,
}

impl GitLabClient {
    /// Creates a new GitLab client.
    ///
    /// # Arguments
    /// * `config` - GitLab configuration with URL and token
    pub fn new(config: &GitLabConfig) -> Self {
        let client = Client::new();

        // Remove trailing slash if present
        let base_url = config.url.trim_end_matches('/').to_string();

        info!("GitLab client initialized for: {}", base_url);

        Self {
            client,
            base_url,
            token: config.token.clone(),
        }
    }

    /// Executes an authenticated GET request.
    async fn get<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T, GitLabError> {
        let url = format!("{}/api/v4{}", self.base_url, endpoint);
        debug!("GitLab API GET: {}", url);

        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(GitLabError::Api {
                status: status.as_u16(),
                message,
            });
        }

        response
            .json::<T>()
            .await
            .map_err(|e| GitLabError::Parse(format!("JSON parsing failed: {}", e)))
    }

    /// Fetches all projects from the GitLab instance (with pagination).
    ///
    /// Uses `membership=true` to only fetch projects the token has access to.
    pub async fn get_all_projects(&self) -> Result<Vec<Project>, GitLabError> {
        let mut all_projects = Vec::new();
        let mut page = 1;
        let per_page = 100;

        loop {
            let endpoint = format!(
                "/projects?membership=true&simple=true&per_page={}&page={}",
                per_page, page
            );

            let projects: Vec<Project> = self.get(&endpoint).await?;
            let count = projects.len();

            debug!("Page {}: {} projects loaded", page, count);
            all_projects.extend(projects);

            // If fewer than per_page are returned, we're done
            if count < per_page {
                break;
            }
            page += 1;
        }

        info!("Total {} projects loaded", all_projects.len());
        Ok(all_projects)
    }

    /// Fetches pipelines of a project with a specific status.
    ///
    /// # Arguments
    /// * `project_id` - ID of the project
    /// * `status` - Pipeline status (pending, running, etc.)
    pub async fn get_pipelines_by_status(
        &self,
        project_id: u64,
        status: &str,
    ) -> Result<Vec<Pipeline>, GitLabError> {
        let endpoint = format!(
            "/projects/{}/pipelines?status={}&per_page=100",
            project_id, status
        );

        self.get(&endpoint).await
    }

    /// Checks if there are active pipelines (pending or running).
    ///
    /// Searches all projects and returns all found active pipelines.
    ///
    /// # Returns
    /// * `Ok(Vec<ActivePipeline>)` - List of active pipelines (may be empty)
    pub async fn find_active_pipelines(&self) -> Result<Vec<ActivePipeline>, GitLabError> {
        let projects = self.get_all_projects().await?;
        let mut active_pipelines = Vec::new();

        for project in projects {
            // Check pending pipelines
            match self.get_pipelines_by_status(project.id, "pending").await {
                Ok(pipelines) => {
                    for pipeline in pipelines {
                        debug!(
                            "Pending pipeline found: {} in {}",
                            pipeline.id, project.path_with_namespace
                        );
                        active_pipelines.push(ActivePipeline {
                            project: project.clone(),
                            pipeline,
                        });
                    }
                }
                Err(e) => {
                    warn!(
                        "Error fetching pending pipelines for {}: {}",
                        project.path_with_namespace, e
                    );
                }
            }

            // Check running pipelines
            match self.get_pipelines_by_status(project.id, "running").await {
                Ok(pipelines) => {
                    for pipeline in pipelines {
                        debug!(
                            "Running pipeline found: {} in {}",
                            pipeline.id, project.path_with_namespace
                        );
                        active_pipelines.push(ActivePipeline {
                            project: project.clone(),
                            pipeline,
                        });
                    }
                }
                Err(e) => {
                    warn!(
                        "Error fetching running pipelines for {}: {}",
                        project.path_with_namespace, e
                    );
                }
            }
        }

        if active_pipelines.is_empty() {
            info!("No active pipelines found");
        } else {
            info!("{} active pipeline(s) found", active_pipelines.len());
        }

        Ok(active_pipelines)
    }

    // NOTE: `has_active_pipelines()` was removed - not needed in this context,
    // as `find_active_pipelines()` already provides the complete information.
}
