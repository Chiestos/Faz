# Kaku

Kaku is a native terminal whose assistant and external coding tools can share user-selected AI connections while retaining distinct runtimes.

## Language

**Kaku Assistant**:
Kaku's built-in conversational agent, including its conversations, tools, approval flow, and terminal-aware behaviors.
_Avoid_: Codex runtime, external coding tool

**Codex Following Mode**:
A Kaku Assistant connection mode that follows the complete effective Codex authentication and model-provider connection, including custom-provider connection semantics, while retaining the Kaku Assistant runtime.
_Avoid_: Codex login reuse, embedded Codex, Codex runtime

**User Codex Configuration**:
The persistent Codex authentication and model-provider connection under `CODEX_HOME`, falling back to the user's `.codex` directory, and excluding project-specific configuration, named profiles, and command-line overrides.
_Avoid_: Project Codex configuration, transient Codex configuration, pane configuration
