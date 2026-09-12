//! Permission engine for tool execution.
//!
//! Policy: read-only operations are auto-approved; mutating operations
//! (`write_file`, `edit_file`, non-allowlisted shell commands) require a
//! grant. Grants carry a scope with a TTL so trust decays over time.
//! Scope rules:
//!
//! - `Once`  — single execution, consumed immediately (in-memory)
//! - `Session` — in-memory, expires quickly (30 min), never persisted
//! - `Project` — persisted with the project, 30 days
//! - `Global`  — persisted app-wide, 30 days
//!
//! Approvals control *whether/when* a tool runs — never *where* it may touch
//! (that is the `sandbox::PathJail`'s job, decided by the project allowlist).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// What identifies a potentially-dangerous call. `command` carries the shell
/// command's head token (e.g. `git`) so grants can be scoped per command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalKey {
    pub tool: String,
    /// For `exec`-style tools: the command head (e.g. "git"), normalized
    /// lowercase. `None` for non-shell tools.
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Once,
    Session,
    Project,
    Global,
}

impl Scope {
    pub fn ttl_secs(self) -> Option<u64> {
        const SESSION: u64 = 30 * 60; // 30 minutes
        const LONG: u64 = 30 * 24 * 60 * 60; // 30 days
        match self {
            Scope::Once => None,
            Scope::Session => Some(SESSION),
            Scope::Project | Scope::Global => Some(LONG),
        }
    }

    pub fn persistable(self) -> bool {
        matches!(self, Scope::Project | Scope::Global)
    }
}

/// A granted approval. `command` must match the key exactly when present.
/// `expires` is a Unix timestamp (None = single-use). `project` tags the
/// project the grant was made in (None = global-scope or legacy in-memory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub scope: Scope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Execute without prompting.
    Allowed,
    /// No covering grant — surface an approval prompt in the UI.
    NeedsApproval,
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Default)]
pub struct PermissionEngine {
    grants: std::sync::Mutex<Vec<Grant>>,
}

impl PermissionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a grant. `Once` grants are consumed on first matching check.
    pub fn grant(&self, mut grant: Grant) {
        grant.expires = grant
            .scope
            .ttl_secs()
            .map(|ttl| now_unix() + ttl as i64)
            .or(grant.expires);
        let mut grants = self.grants.lock().unwrap();
        grants.push(grant);
    }

    /// Does an active grant cover this key in this project? Read-only tools
    /// (empty command head matches nothing here) are handled by the caller:
    /// only tools that produce an `ApprovalKey` consult the engine.
    /// `project` is the current project id (None when no project is active).
    /// Global grants apply everywhere; anything else must match the project
    /// it was made in, so trust never leaks across projects.
    pub fn check(&self, key: &ApprovalKey, project: Option<&str>) -> Decision {
        let now = now_unix();
        let mut grants = self.grants.lock().unwrap();
        grants.retain(|g| g.expires.is_none_or(|e| e > now));
        // Match: same tool, and command matches exactly when both present. A
        // tool-wide grant (no command) covers any call for that tool, but a
        // command-scoped grant never covers a bare (unspecified) call.
        let hit = grants.iter().position(|g| {
            g.tool == key.tool
                && match (&g.command, &key.command) {
                    (Some(c), Some(k)) => c.eq_ignore_ascii_case(k),
                    (None, Some(_)) => true,
                    (Some(_), None) => false,
                    (None, None) => true,
                }
                && match (&g.project, project) {
                    (None, _) => g.scope == Scope::Global,
                    (Some(p), Some(cur)) => p == cur,
                    (Some(_), None) => false,
                }
        });
        match hit {
            Some(idx) => {
                let grant = &grants[idx];
                if grant.scope == Scope::Once {
                    grants.remove(idx); // consume
                }
                Decision::Allowed
            }
            None => Decision::NeedsApproval,
        }
    }

    /// Expire-check only (no consumption), used to display grant state.
    pub fn has_grant(&self, key: &ApprovalKey, project: Option<&str>) -> bool {
        self.check(key, project) == Decision::Allowed
    }

    /// Grants that should be persisted with the project / global config.
    pub fn persistable(&self) -> Vec<Grant> {
        let now = now_unix();
        self.grants
            .lock()
            .unwrap()
            .iter()
            .filter(|g| g.scope.persistable() && g.expires.is_none_or(|e| e > now))
            .cloned()
            .collect()
    }

    /// Restore persisted grants (expired ones are dropped on next check).
    pub fn load(&self, grants: Vec<Grant>) {
        self.grants.lock().unwrap().extend(grants);
    }

    /// Drop in-memory grants tagged for `project` (used on project switch
    /// after saving; global grants are kept).
    pub fn evict_project(&self, project: &str) {
        self.grants
            .lock()
            .unwrap()
            .retain(|g| g.project.as_deref() != Some(project));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tool: &str, command: Option<&str>) -> ApprovalKey {
        ApprovalKey {
            tool: tool.to_string(),
            command: command.map(String::from),
        }
    }

    #[test]
    fn no_grant_needs_approval() {
        let engine = PermissionEngine::new();
        assert_eq!(engine.check(&key("write_file", None), Some("p")), Decision::NeedsApproval);
    }

    #[test]
    fn once_grant_is_consumed() {
        let engine = PermissionEngine::new();
        engine.grant(Grant {
            tool: "write_file".into(),
            command: None,
            scope: Scope::Once,
            expires: None,
            project: Some("p".into()),
        });
        assert_eq!(engine.check(&key("write_file", None), Some("p")), Decision::Allowed);
        // Consumed — next call needs approval again.
        assert_eq!(engine.check(&key("write_file", None), Some("p")), Decision::NeedsApproval);
    }

    #[test]
    fn session_grant_repeats() {
        let engine = PermissionEngine::new();
        engine.grant(Grant {
            tool: "exec".into(),
            command: Some("npm".into()),
            scope: Scope::Session,
            expires: None,
            project: Some("p".into()),
        });
        assert_eq!(engine.check(&key("exec", Some("npm")), Some("p")), Decision::Allowed);
        assert_eq!(engine.check(&key("exec", Some("npm")), Some("p")), Decision::Allowed);
        // Different command head does not match.
        assert_eq!(engine.check(&key("exec", Some("cargo")), Some("p")), Decision::NeedsApproval);
        // Grant for a specific command must not cover a bare key.
        assert_eq!(engine.check(&key("exec", None), Some("p")), Decision::NeedsApproval);
    }

    #[test]
    fn grants_do_not_leak_across_projects() {
        let engine = PermissionEngine::new();
        engine.grant(Grant {
            tool: "write_file".into(),
            command: None,
            scope: Scope::Project,
            expires: None,
            project: Some("a".into()),
        });
        assert_eq!(engine.check(&key("write_file", None), Some("a")), Decision::Allowed);
        assert_eq!(engine.check(&key("write_file", None), Some("b")), Decision::NeedsApproval);
        assert_eq!(engine.check(&key("write_file", None), None), Decision::NeedsApproval);
        // Global grants apply everywhere.
        engine.grant(Grant {
            tool: "exec".into(),
            command: None,
            scope: Scope::Global,
            expires: None,
            project: None,
        });
        assert_eq!(engine.check(&key("exec", None), Some("a")), Decision::Allowed);
        assert_eq!(engine.check(&key("exec", None), Some("b")), Decision::Allowed);
    }

    #[test]
    fn expired_grant_needs_approval() {
        let engine = PermissionEngine::new();
        engine.grants.lock().unwrap().push(Grant {
            tool: "write_file".into(),
            command: None,
            scope: Scope::Project,
            expires: Some(now_unix() - 1),
            project: Some("p".into()),
        });
        assert_eq!(engine.check(&key("write_file", None), Some("p")), Decision::NeedsApproval);
    }

    #[test]
    fn persistable_filters_session_and_once() {
        let engine = PermissionEngine::new();
        for scope in [Scope::Once, Scope::Session, Scope::Project, Scope::Global] {
            engine.grant(Grant {
                tool: "exec".into(),
                command: Some(format!("{scope:?}")),
                scope,
                expires: None,
                project: Some("p".into()),
            });
        }
        let persisted = engine.persistable();
        assert_eq!(persisted.len(), 2);
        assert!(persisted.iter().all(|g| g.scope.persistable()));
    }

    #[test]
    fn command_match_is_case_insensitive() {
        let engine = PermissionEngine::new();
        engine.grant(Grant {
            tool: "exec".into(),
            command: Some("NPM".into()),
            scope: Scope::Session,
            expires: None,
            project: Some("p".into()),
        });
        assert_eq!(engine.check(&key("exec", Some("npm")), Some("p")), Decision::Allowed);
    }
}
