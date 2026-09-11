//! Shared enums and value types. String forms match the SQL `CHECK`
//! constraints in the schema (§10.4) exactly.

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum!(
    /// Command kind (§14). Ordering is classification priority.
    CommandKind { Test => "test", Build => "build", Lint => "lint", Git => "git", Other => "other" }
);

string_enum!(
    /// Command outcome (§14).
    Outcome { Pass => "PASS", Fail => "FAIL", Interrupted => "INTERRUPTED", Unknown => "UNKNOWN" }
);

string_enum!(
    /// Edit lifecycle status (§13).
    EditStatus {
        Active => "ACTIVE",
        Reverted => "REVERTED",
        Discarded => "DISCARDED",
        Committed => "COMMITTED",
        Reapplied => "REAPPLIED",
    }
);

string_enum!(
    /// How an edit was undone (§13.2).
    Mechanism {
        GitCommand => "git_command",
        InverseEdit => "inverse_edit",
        Rewrite => "rewrite",
        External => "external",
    }
);

string_enum!(
    /// Origin of a file version observation.
    VersionSource {
        PreEdit => "pre_edit",
        PostEdit => "post_edit",
        Original => "original",
        GitPre => "git_pre",
        GitPost => "git_post",
        TurnScan => "turn_scan",
    }
);

string_enum!(
    /// Intent level (§12).
    IntentLevel { Root => "ROOT", Subtask => "SUBTASK", Latest => "LATEST" }
);

string_enum!(
    /// Continuation delivery state (§15.1).
    ContinuationState {
        Pending => "PENDING",
        Attached => "ATTACHED",
        Confirmed => "CONFIRMED",
        Superseded => "SUPERSEDED",
        Expired => "EXPIRED",
    }
);

string_enum!(
    /// Continuation delivery channel (§15.3).
    Channel { SessionStart => "session_start", PostTool => "post_tool", UserPrompt => "user_prompt" }
);

string_enum!(
    /// Checkpoint trigger.
    Trigger { Manual => "manual", Auto => "auto", Cli => "cli" }
);

impl ContinuationState {
    /// PENDING and ATTACHED are "live" (§15.1).
    pub fn is_live(self) -> bool {
        matches!(
            self,
            ContinuationState::Pending | ContinuationState::Attached
        )
    }
}

/// Hook event names as they appear in the `events.hook_event` column.
pub mod hook_event {
    pub const SESSION_START: &str = "SessionStart";
    pub const USER_PROMPT_SUBMIT: &str = "UserPromptSubmit";
    pub const PRE_TOOL_USE: &str = "PreToolUse";
    pub const POST_TOOL_USE: &str = "PostToolUse";
    pub const POST_TOOL_USE_FAILURE: &str = "PostToolUseFailure";
    pub const STOP: &str = "Stop";
    pub const PRE_COMPACT: &str = "PreCompact";
    pub const POST_COMPACT: &str = "PostCompact";
    pub const SESSION_END: &str = "SessionEnd";
    /// Recorded when stdin could not be parsed but a session id was found.
    pub const MALFORMED: &str = "malformed";
    /// Spooled when `pre-compact` could not insert its checkpoint.
    pub const CHECKPOINT_REQUEST: &str = "checkpoint_request";
}

/// Tool classification used by normalization and the reducer.
pub mod tools {
    pub const EDIT_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];
    pub const SHELL_TOOLS: &[&str] = &["Bash", "PowerShell"];

    pub fn is_edit(tool: &str) -> bool {
        EDIT_TOOLS.contains(&tool)
    }

    pub fn is_shell(tool: &str) -> bool {
        SHELL_TOOLS.contains(&tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for k in CommandKind::ALL {
            assert_eq!(CommandKind::parse(k.as_str()), Some(*k));
        }
        for s in ContinuationState::ALL {
            assert_eq!(ContinuationState::parse(s.as_str()), Some(*s));
        }
        assert!(CommandKind::Test < CommandKind::Git);
        assert_eq!(Outcome::Fail.to_string(), "FAIL");
    }
}
