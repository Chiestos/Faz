use anyhow::Context;
use clap::Parser;
use serde_json::{json, Value};
use std::io::{Read, Write};

pub(crate) const HOOK_COMMAND: &str = "kaku claude-status-hook";

#[derive(Debug, Parser, Clone, Default)]
pub struct ClaudeStatusHookCommand {}

impl ClaudeStatusHookCommand {
    pub fn run(&self) -> anyhow::Result<()> {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .context("read Claude hook input")?;
        let Ok(input) = serde_json::from_str::<Value>(&input) else {
            return Ok(());
        };
        let Some(sequence) = terminal_sequence_for_hook(&input) else {
            return Ok(());
        };

        serde_json::to_writer(std::io::stdout(), &json!({ "terminalSequence": sequence }))
            .context("write Claude hook output")?;
        std::io::stdout().write_all(b"\n")?;
        Ok(())
    }
}

fn progress_sequence(state: u8) -> String {
    format!("\x1b]9;4;{state}\x07")
}

fn notification_sequence(message: &str) -> String {
    format!("\x1b]9;{message}\x07")
}

fn has_background_activity(input: &Value) -> bool {
    ["background_tasks", "session_crons"].iter().any(|key| {
        input
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    })
}

fn terminal_sequence_for_hook(input: &Value) -> Option<String> {
    let event = input.get("hook_event_name")?.as_str()?;
    let sequence = match event {
        "SessionStart" | "SessionEnd" => progress_sequence(0),
        "UserPromptSubmit"
        | "UserPromptExpansion"
        | "PostToolUse"
        | "PostToolUseFailure"
        | "PostToolBatch"
        | "ElicitationResult"
        | "SubagentStart"
        | "SubagentStop"
        | "TaskCreated"
        | "TaskCompleted" => progress_sequence(3),
        "PermissionRequest" => {
            progress_sequence(4) + &notification_sequence("Claude Code needs permission")
        }
        "Elicitation" => {
            progress_sequence(4) + &notification_sequence("Claude Code needs your input")
        }
        "Stop" if has_background_activity(input) => progress_sequence(3),
        "Stop" => progress_sequence(0) + &notification_sequence("Claude Code finished"),
        "StopFailure" => {
            progress_sequence(4) + &notification_sequence("Claude Code stopped with an error")
        }
        "Notification" => match input.get("notification_type").and_then(Value::as_str) {
            Some(
                "permission_prompt"
                | "idle_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input",
            ) => progress_sequence(4),
            Some("auth_success" | "elicitation_complete" | "elicitation_response") => {
                progress_sequence(3)
            }
            Some("agent_completed") => {
                progress_sequence(0) + &notification_sequence("Claude Code agent finished")
            }
            _ => return None,
        },
        _ => return None,
    };
    Some(sequence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_and_permission_map_to_running_and_attention() {
        assert_eq!(
            terminal_sequence_for_hook(&json!({ "hook_event_name": "UserPromptSubmit" })),
            Some("\x1b]9;4;3\x07".to_string())
        );
        assert_eq!(
            terminal_sequence_for_hook(&json!({ "hook_event_name": "PermissionRequest" })),
            Some("\x1b]9;4;4\x07\x1b]9;Claude Code needs permission\x07".to_string())
        );
    }

    #[test]
    fn stop_keeps_running_while_background_work_exists() {
        assert_eq!(
            terminal_sequence_for_hook(&json!({
                "hook_event_name": "Stop",
                "background_tasks": [{ "id": "task-1" }],
                "session_crons": []
            })),
            Some("\x1b]9;4;3\x07".to_string())
        );
    }

    #[test]
    fn completed_and_failed_turns_emit_distinct_states() {
        assert_eq!(
            terminal_sequence_for_hook(&json!({
                "hook_event_name": "Stop",
                "background_tasks": [],
                "session_crons": []
            })),
            Some("\x1b]9;4;0\x07\x1b]9;Claude Code finished\x07".to_string())
        );
        assert_eq!(
            terminal_sequence_for_hook(&json!({ "hook_event_name": "StopFailure" })),
            Some("\x1b]9;4;4\x07\x1b]9;Claude Code stopped with an error\x07".to_string())
        );
    }
}
