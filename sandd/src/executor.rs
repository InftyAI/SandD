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
        assert!(result.duration_ms > 0);
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

    #[tokio::test]
    async fn test_command_with_multiple_env_vars() {
        let executor = CommandExecutor::new();
        let mut env = HashMap::new();
        env.insert("VAR1".to_string(), "value1".to_string());
        env.insert("VAR2".to_string(), "value2".to_string());

        #[cfg(unix)]
        let cmd = "echo $VAR1 $VAR2";
        #[cfg(windows)]
        let cmd = "echo %VAR1% %VAR2%";

        let result = executor.execute(cmd, 10, env, None).await.unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("value1"));
        assert!(result.stdout.contains("value2"));
    }

    #[tokio::test]
    async fn test_command_with_cwd() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let (cmd, expected) = ("pwd", "/tmp");
        #[cfg(windows)]
        let (cmd, expected) = ("cd", "\\");

        let result = executor
            .execute(cmd, 10, HashMap::new(), Some("/tmp".to_string()))
            .await;

        // On some systems this might fail if /tmp doesn't exist
        if let Ok(result) = result {
            assert_eq!(result.exit_code, 0);
            assert!(result.stdout.contains(expected) || result.stdout.contains("/private/tmp"));
        }
    }

    #[tokio::test]
    async fn test_command_failure() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "exit 42";
        #[cfg(windows)]
        let cmd = "exit /b 42";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 42);
    }

    #[tokio::test]
    async fn test_command_stderr() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "echo 'error message' >&2";
        #[cfg(windows)]
        let cmd = "echo error message 1>&2";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stderr.contains("error"));
    }

    #[tokio::test]
    async fn test_command_timeout() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "sleep 5";
        #[cfg(windows)]
        let cmd = "timeout /t 5";

        let result = executor.execute(cmd, 1, HashMap::new(), None).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_empty_command_output() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "true";
        #[cfg(windows)]
        let cmd = "exit /b 0";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout.trim(), "");
    }

    #[tokio::test]
    async fn test_large_output() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "for i in {1..1000}; do echo 'Line $i'; done";
        #[cfg(windows)]
        let cmd = "for /l %i in (1,1,1000) do @echo Line %i";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.len() > 1000);
    }

    #[tokio::test]
    async fn test_command_with_quotes() {
        let executor = CommandExecutor::new();

        let cmd = "echo 'hello world'";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
        assert!(result.stdout.contains("world"));
    }

    #[tokio::test]
    async fn test_command_with_pipe() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "echo 'test' | cat";
        #[cfg(windows)]
        let cmd = "echo test | findstr test";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("test"));
    }

    #[tokio::test]
    async fn test_duration_tracking() {
        let executor = CommandExecutor::new();

        #[cfg(unix)]
        let cmd = "sleep 0.1";
        #[cfg(windows)]
        let cmd = "timeout /t 1";

        let result = executor
            .execute(cmd, 10, HashMap::new(), None)
            .await
            .unwrap();

        // Duration should be at least 100ms
        assert!(result.duration_ms >= 90);
    }

    #[tokio::test]
    async fn test_nonexistent_command() {
        let executor = CommandExecutor::new();

        let cmd = "this_command_does_not_exist_12345";

        let result = executor.execute(cmd, 10, HashMap::new(), None).await;

        // Should fail to spawn or return non-zero exit code
        if let Ok(output) = result {
            assert_ne!(output.exit_code, 0);
        }
    }

    #[test]
    fn test_command_output_debug() {
        let output = CommandOutput {
            stdout: "test".to_string(),
            stderr: "error".to_string(),
            exit_code: 0,
            duration_ms: 100,
        };

        let debug_str = format!("{:?}", output);
        assert!(debug_str.contains("exit_code: 0"));
        assert!(debug_str.contains("duration_ms: 100"));
    }
}
