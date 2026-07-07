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

/// A GitLab job.
///
/// NOTE: Only `id`, `status`, and `tag_list` are relevant here.
#[derive(Debug, Deserialize, Clone)]
pub struct Job {
    /// Job ID
    pub id: u64,
    /// Status (pending, running, success, failed, etc.)
    pub status: String,
    /// Runner tags assigned to this job
    #[serde(default)]
    pub tag_list: Vec<String>,
}

/// Information about an active job with project context.
#[derive(Debug, Clone)]
pub struct ActiveJob {
    /// The project the job belongs to
    pub project: Project,
    /// The job itself
    pub job: Job,
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
            // Only check projects that are starred and not archived 
            let endpoint = format!(
                "/projects?membership=true&simple=true&archived=false&starred=true&per_page={}&page={}",
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

    /// Fetches jobs of a project filtered by scope (e.g., "pending", "running").
    ///
    /// Uses the Jobs API which includes `tag_list` per job, unlike the Pipelines API.
    /// Requires only `read_api` scope.
    async fn get_jobs_by_scope(
        &self,
        project_id: u64,
        scope: &str,
    ) -> Result<Vec<Job>, GitLabError> {
        let endpoint = format!(
            "/projects/{}/jobs?scope[]={}&per_page=100",
            project_id, scope
        );

        self.get(&endpoint).await
    }

    /// Searches all projects for active jobs (pending or running).
    ///
    /// When `tag_filter` is non-empty, only jobs whose `tag_list` contains at least
    /// one of the specified tags are returned. This is the mechanism for restricting
    /// runner creation to specific job types without requiring additional API scopes.
    ///
    /// # Returns
    /// * `Ok(Vec<ActiveJob>)` - List of matching active jobs (may be empty)
    pub async fn find_active_jobs(
        &self,
        tag_filter: Option<&[String]>,
    ) -> Result<Vec<ActiveJob>, GitLabError> {
        let projects = self.get_all_projects().await?;
        let mut active_jobs = Vec::new();

        if let Some(tags) = tag_filter {
            debug!("Tag filter active: {:?}", tags);
        }

        for project in projects {
            for scope in &["pending", "running"] {
                match self.get_jobs_by_scope(project.id, scope).await {
                    Ok(jobs) => {
                        for job in jobs {
                            if let Some(tags) = tag_filter {
                                if !job.tag_list.iter().any(|t| tags.contains(t)) {
                                    debug!(
                                        "Skipping {} job {} in {} (tags: {:?} don't match filter)",
                                        scope, job.id, project.path_with_namespace, job.tag_list
                                    );
                                    continue;
                                }
                            }
                            debug!(
                                "{} job found: {} in {} (tags: {:?})",
                                scope, job.id, project.path_with_namespace, job.tag_list
                            );
                            active_jobs.push(ActiveJob {
                                project: project.clone(),
                                job,
                            });
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Error fetching {} jobs for {}: {}",
                            scope, project.path_with_namespace, e
                        );
                    }
                }
            }
        }

        if active_jobs.is_empty() {
            info!("No active jobs found");
        } else {
            info!("{} active job(s) found", active_jobs.len());
        }

        Ok(active_jobs)
    }
}
