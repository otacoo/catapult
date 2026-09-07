//! Git worktree orchestration (Phase 5): run parallel agent sessions on
//! separate branches without context bleeding. Thin wrappers over `git
//! worktree` with output parsing; failures are loud so the UI can surface
//! them (e.g. a worktree with uncommitted changes needs `--force`).

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

const GIT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub bare: bool,
    /// The main worktree of the repo (first entry in `git worktree list`).
    pub main: bool,
}

/// Run `git` with the repo as CWD, with a timeout. Returns stdout on success.
fn run_git(args: &[&str], cwd: &Path, timeout_secs: u64) -> Result<String> {
    let mut child = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Read remaining output after exit.
                use std::io::Read;
                let mut out = Vec::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_end(&mut out);
                }
                let mut err = Vec::new();
                if let Some(mut se) = child.stderr.take() {
                    let _ = se.read_to_end(&mut err);
                }
                if !status.success() {
                    let msg = String::from_utf8_lossy(&err).trim().to_string();
                    bail!("git {} failed: {}", args.join(" "), msg);
                }
                return Ok(String::from_utf8_lossy(&out).to_string());
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    bail!("git {} timed out after {timeout_secs}s", args.join(" "));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("Failed while waiting for git: {e}"),
        }
    }
}

/// Is this path inside a git working tree?
pub fn is_repo(path: &Path) -> bool {
    run_git(&["rev-parse", "--is-inside-work-tree"], path, 10)
        .map(|out| out.trim() == "true")
        .unwrap_or(false)
}

/// Parse `git worktree list --porcelain` output into entries.
pub fn parse_worktree_list(output: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            if let Some(wt) = current.take() {
                out.push(wt);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(wt) = current.take() {
                out.push(wt);
            }
            current = Some(Worktree {
                path: path.trim().to_string(),
                branch: None,
                head: None,
                bare: false,
                main: false,
            });
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            if let Some(wt) = current.as_mut() {
                wt.head = Some(head.trim().to_string());
            }
        } else if let Some(branch) = line.strip_prefix("branch ") {
            if let Some(wt) = current.as_mut() {
                let name = branch
                    .trim()
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch.trim())
                    .to_string();
                wt.branch = Some(name);
            }
        } else if line == "bare" {
            if let Some(wt) = current.as_mut() {
                wt.bare = true;
            }
        } else if line == "detached" {
            if let Some(wt) = current.as_mut() {
                wt.branch = None;
            }
        }
    }
    if let Some(wt) = current {
        out.push(wt);
    }
    if let Some(first) = out.first_mut() {
        first.main = true;
    }
    out
}

/// List the worktrees of the repo containing `path`.
pub fn list(path: &Path) -> Result<Vec<Worktree>> {
    let out = run_git(&["worktree", "list", "--porcelain"], path, GIT_TIMEOUT_SECS)?;
    Ok(parse_worktree_list(&out))
}

/// Create a worktree at an explicit path. `branch` names a *new* branch to
/// create and check out (the parallel-agent case). Returns the worktree path.
pub fn add(repo: &Path, path: &Path, branch: Option<&str>) -> Result<String> {
    let path_str = path.to_string_lossy().to_string();
    match branch {
        Some(b) if !b.is_empty() => {
            run_git(&["worktree", "add", "-b", b, &path_str], repo, GIT_TIMEOUT_SECS)?;
        }
        _ => {
            run_git(&["worktree", "add", &path_str], repo, GIT_TIMEOUT_SECS)?;
        }
    }
    Ok(path_str)
}

/// Remove a worktree. `force` drops uncommitted changes and locked state —
/// the UI must confirm before passing true.
pub fn remove(repo: &Path, path: &str, force: bool) -> Result<()> {
    if force {
        run_git(&["worktree", "remove", "--force", path], repo, GIT_TIMEOUT_SECS)?;
    } else {
        run_git(&["worktree", "remove", path], repo, GIT_TIMEOUT_SECS)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "worktree C:/code/catapult\n\
HEAD abc123def\n\
branch refs/heads/master\n\
\n\
worktree C:/code/catapult-wip\n\
HEAD 999b\n\
branch refs/heads/feature-x\n\
\n\
worktree C:/code/bare.git\n\
bare\n\
";

    #[test]
    fn parses_porcelain_list() {
        let list = parse_worktree_list(SAMPLE);
        assert_eq!(list.len(), 3);
        assert!(list[0].main);
        assert!(!list[1].main);
        assert!(!list[2].main);
        assert_eq!(list[0].path, "C:/code/catapult");
        assert_eq!(list[0].branch.as_deref(), Some("master"));
        assert_eq!(list[0].head.as_deref(), Some("abc123def"));
        assert_eq!(list[1].branch.as_deref(), Some("feature-x"));
        assert!(list[2].bare);
        assert!(list[2].branch.is_none());
    }

    #[test]
    fn empty_output_is_empty_list() {
        assert!(parse_worktree_list("").is_empty());
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn add_and_remove_roundtrip() {
        if !git_available() {
            return; // git is not installed in this environment
        }
        let root = std::env::temp_dir().join(format!("harness-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        run_git(&["init", "-b", "main"], &root, 30).unwrap();
        run_git(&["config", "user.email", "t@t"], &root, 30).unwrap();
        run_git(&["config", "user.name", "t"], &root, 30).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        run_git(&["add", "."], &root, 30).unwrap();
        run_git(&["commit", "-m", "init", "--no-gpg-sign"], &root, 30).unwrap();

        assert!(is_repo(&root));
        let wt_path = root.parent().unwrap().join(format!("harness-wt-child-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt_path);
        add(&root, &wt_path, Some("feature-x")).unwrap();
        let entries = list(&root).unwrap();
        assert!(entries.iter().any(|w| w.branch.as_deref() == Some("feature-x")));
        remove(&root, &wt_path.to_string_lossy(), false).unwrap();
        let entries = list(&root).unwrap();
        assert!(!entries.iter().any(|w| w.branch.as_deref() == Some("feature-x")));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&wt_path);
    }
}
