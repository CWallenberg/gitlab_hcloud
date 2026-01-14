//! Cloud-Init template generator.
//!
//! Generates the cloud-init configuration for the GitLab Runner server,
//! based on the Terraform template.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use tracing::info;

/// Docker-Compose configuration - included at compile time.
const DOCKER_COMPOSE: &str = include_str!("../assets/docker-compose.yml");

/// Generates the cloud-init configuration for the runner server.
///
/// The configuration:
/// 1. Updates packages
/// 2. Writes the runner.toml configuration
/// 3. Writes the docker-compose.yml
/// 4. Installs Docker
/// 5. Starts the GitLab Runner container
///
/// # Arguments
/// * `runner_config` - Contents of the runner.toml file
///
/// # Returns
/// The complete cloud-init configuration as a string
pub fn generate_cloud_init(runner_config: &str) -> String {
    info!("Generating cloud-init configuration");

    // Base64 encode runner config
    let runner_config_b64 = BASE64.encode(runner_config.as_bytes());

    // Base64 encode docker-compose
    let docker_compose_b64 = BASE64.encode(DOCKER_COMPOSE.as_bytes());

    // Generate cloud-init YAML
    let cloud_init = format!(
        r#"#cloud-config
package_update: true
package_upgrade: true

write_files:
  - path: /srv/gitlab-runner/docker-compose.yml
    encoding: b64
    content: {docker_compose_b64}
  - path: /srv/gitlab-runner/config/config.toml
    encoding: b64
    content: {runner_config_b64}

runcmd:
  - curl -fsSL https://get.docker.com -o install-docker.sh
  - sh install-docker.sh
  - docker compose -f /srv/gitlab-runner/docker-compose.yml up -d
"#,
        docker_compose_b64 = docker_compose_b64,
        runner_config_b64 = runner_config_b64,
    );

    info!(
        "Cloud-init configuration generated ({} bytes)",
        cloud_init.len()
    );
    cloud_init
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_cloud_init() {
        let runner_config = "concurrent = 1\ncheck_interval = 0";
        let result = generate_cloud_init(runner_config);

        assert!(result.starts_with("#cloud-config"));
        assert!(result.contains("package_update: true"));
        assert!(result.contains("docker compose"));
    }
}
