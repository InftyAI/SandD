use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

pub struct CommandExecutor;

#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

impl CommandExecutor {
    pub fn new() -> Self {
        Self
    }

    pub async fn execute(
        &self,
        command: &str,
        timeout_secs: u64,
        env: HashMap<String, String>,
        cwd: Option<String>,
    ) -> Result<CommandOutput> {
        let start = Instant::now();

        debug!("Executing: {}", command);

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };

        // Set environment variables
        for (key, value) in env {
            cmd.env(key, value);
        }

        // Set working directory
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let child = cmd.spawn().context("Failed to spawn command")?;

        // Wait for completion with timeout
        let output = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
            .context("Command timed out")?
            .context("Failed to wait for command")?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simple_command() {
        let executor = CommandExecutor::new();
        let result = executor
            .execute("echo hello", 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_command_with_env() {
        let executor = CommandExecutor::new();
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());

        #[cfg(unix)]
        let cmd = "echo $TEST_VAR";
        #[cfg(windows)]
        let cmd = "echo %TEST_VAR%";

        let result = executor.execute(cmd, 10, env, None).await.unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("test_value"));
    }
}
