//! Path jail: the hard sandbox boundary for all file-accessing tools.
//!
//! Every tool that takes a path (`read_file`, `write_file`, `edit_file`,
//! `find_files`, `search_content`, attachments) resolves its arguments through
//! a `PathJail` rooted at the project's working directory. Anything that does
//! not resolve *inside* the jail is rejected — including `..` traversal and
//! symlink/junction escapes, because resolution is canonical before the check.
//!
//! Escape policy: approvals control *whether/when* a tool runs, never *where*
//! it may touch. Escapes must be pre-declared in the per-project allowlist:
//! `extra_read` entries are readable but never writable; `extra_write` allows
//! writes. (Decision recorded in `plan.md` §5.7.)

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Classification of a path against a jail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathScope {
    /// Inside the project root: read and write allowed.
    Project,
    /// Pre-declared read-only extra path: read allowed, write denied.
    ExtraRead,
    /// Pre-declared writable path outside the project root.
    ExtraWrite,
    /// Outside every allowed scope.
    Denied,
}

/// Normalize a canonical path for comparison: strip the Windows verbatim
/// prefix (`\\?\`) that `canonicalize` produces, and (on Windows) lowercase
/// so comparisons are case-insensitive like the filesystem itself.
fn normalize(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if cfg!(windows) {
        PathBuf::from(text.to_lowercase())
    } else {
        PathBuf::from(text.to_owned())
    }
}

/// Canonicalize `path`, falling back to canonicalizing the parent directory
/// for targets that do not exist yet (write targets). The final component is
/// re-attached verbatim, so new files are jailed by their parent directory.
fn canonicalize_target(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let file_name = path
                .file_name()
                .with_context(|| format!("Path has no final component: {}", path.display()))?;
            let parent = path
                .parent()
                .with_context(|| format!("Path has no parent: {}", path.display()))?;
            let parent = std::fs::canonicalize(parent)
                .with_context(|| format!("Parent directory does not exist: {}", parent.display()))?;
            Ok(parent.join(file_name))
        }
        Err(e) => Err(e).with_context(|| format!("Cannot resolve path {}", path.display())),
    }
}

/// Component-boundary prefix test: `dir` contains `path`. Trailing components
/// are preserved; `H:\a\b` does NOT match a dir `H:\projects\foobar`.
fn starts_with_dir(dir: &Path, path: &Path) -> bool {
    path.strip_prefix(dir).is_ok()
}

/// Collapse `.`/`..` lexically without touching the filesystem. Used for the
/// *user-visible* form of a rejected path; safety checks always run on the
/// canonicalized (symlink-resolved) form.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The jail: canonical project root plus pre-declared extra scopes.
#[derive(Debug, Clone)]
pub struct PathJail {
    /// Canonical, case-normalized project root.
    root: PathBuf,
    /// Canonical, case-normalized read-only extra roots.
    extra_read: Vec<PathBuf>,
    /// Canonical, case-normalized writable extra roots.
    extra_write: Vec<PathBuf>,
}

impl PathJail {
    /// Build a jail. All roots must exist (they are canonicalized here).
    pub fn new(root: &Path, extra_read: &[PathBuf], extra_write: &[PathBuf]) -> Result<Self> {
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("Project root does not exist: {}", root.display()))?;
        if !root.is_dir() {
            bail!("Project root is not a directory: {}", root.display());
        }
        let mut extra_r = Vec::new();
        for p in extra_read {
            extra_r.push(
                std::fs::canonicalize(p)
                    .with_context(|| format!("Extra read path does not exist: {}", p.display()))?,
            );
        }
        let mut extra_w = Vec::new();
        for p in extra_write {
            extra_w.push(
                std::fs::canonicalize(p)
                    .with_context(|| format!("Extra write path does not exist: {}", p.display()))?,
            );
        }
        Ok(Self {
            root: normalize(&root),
            extra_read: extra_r.iter().map(|p| normalize(p)).collect(),
            extra_write: extra_w.iter().map(|p| normalize(p)).collect(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn scope(&self, canonical: &Path) -> PathScope {
        let norm = normalize(canonical);
        if starts_with_dir(&self.root, &norm) {
            return PathScope::Project;
        }
        if self.extra_write.iter().any(|d| starts_with_dir(d, &norm)) {
            return PathScope::ExtraWrite;
        }
        if self.extra_read.iter().any(|d| starts_with_dir(d, &norm)) {
            return PathScope::ExtraRead;
        }
        PathScope::Denied
    }

    /// Validate a read target. Returns the canonical, resolved path.
    pub fn check_read(&self, path: &Path) -> Result<PathBuf> {
        let canonical = canonicalize_target(path)?;
        match self.scope(&canonical) {
            PathScope::Project | PathScope::ExtraRead | PathScope::ExtraWrite => Ok(canonical),
            PathScope::Denied => bail!(
                "Read denied: '{}' is outside the sandboxed project and not in the read allowlist",
                lexical_normalize(path).display()
            ),
        }
    }

    /// Validate a write target. Returns the canonical, resolved path. The
    /// target itself may not exist yet; its parent must. Extra-read roots are
    /// read-only by definition and never writable.
    pub fn check_write(&self, path: &Path) -> Result<PathBuf> {
        let canonical = canonicalize_target(path)?;
        match self.scope(&canonical) {
            PathScope::Project | PathScope::ExtraWrite => Ok(canonical),
            PathScope::ExtraRead => bail!(
                "Write denied: '{}' is a read-only allowlisted path",
                lexical_normalize(path).display()
            ),
            PathScope::Denied => bail!(
                "Write denied: '{}' is outside the sandboxed project and not in the write allowlist",
                lexical_normalize(path).display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("harness-jail-{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn jail_from(root: &Path) -> PathJail {
        PathJail::new(root, &[], &[]).unwrap()
    }

    #[test]
    fn paths_inside_project_are_writable() {
        let root = temp_dir("inside");
        let jail = jail_from(&root);
        let target = root.join("sub").join("new.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let ok = jail.check_write(&target).unwrap();
        assert_eq!(ok.file_name().unwrap(), "new.txt");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traversal_outside_root_is_denied() {
        let root = temp_dir("traversal");
        let outside = root.parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "x").unwrap();
        let jail = jail_from(&root);
        assert!(jail.check_read(&outside).is_err());
        assert!(jail.check_write(&root.join("..").join("escape.txt")).is_err());
        std::fs::remove_file(&outside).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_to_missing_parent_is_denied() {
        let root = temp_dir("missing-parent");
        let jail = jail_from(&root);
        assert!(jail.check_write(&root.join("no").join("such.txt")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn deep_prefix_lookalike_does_not_match() {
        let root = temp_dir("prefix");
        let jail = jail_from(&root);
        // Sibling directory that shares a string prefix with the root name.
        let sibling = root.parent().unwrap().join(format!(
            "{}x",
            root.file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&sibling).unwrap();
        let target = sibling.join("f.txt");
        assert!(jail.check_write(&target).is_err());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    #[test]
    fn extra_read_allows_read_but_not_write() {
        let root = temp_dir("extra-read");
        let library = temp_dir("extra-read-lib");
        let jail = PathJail::new(&root, &[library.clone()], &[]).unwrap();
        let f = library.join("data.txt");
        std::fs::write(&f, "x").unwrap();
        assert!(jail.check_read(&f).is_ok());
        assert!(jail.check_write(&f).is_err());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&library);
    }

    #[test]
    fn extra_write_allows_both() {
        let root = temp_dir("extra-write");
        let library = temp_dir("extra-write-lib");
        let jail = PathJail::new(&root, &[], &[library.clone()]).unwrap();
        let f = library.join("out.txt");
        assert!(jail.check_write(&f).is_ok());
        assert!(jail.check_read(&f).is_ok());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&library);
    }

    #[test]
    fn case_insensitive_roots_on_windows() {
        let root = temp_dir("case");
        let jail = jail_from(&root);
        let upper: PathBuf = if cfg!(windows) {
            // Flip the case of the first directory component.
            let name = root.file_name().unwrap().to_string_lossy().to_uppercase();
            root.with_file_name(name)
        } else {
            root.to_path_buf()
        };
        let ok = jail.check_write(&upper.join("f.txt")).is_ok();
        assert_eq!(ok, cfg!(windows), "case-insensitivity is a Windows property");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn junction_escape_is_denied() {
        let root = temp_dir("junction");
        let outside = temp_dir("junction-out");
        let link = root.join("door");
        // Junctions do not require admin privileges on Windows.
        let out = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output();
        if !out.map(|o| o.status.success()).unwrap_or(false) {
            return; // junction creation unavailable in this environment
        }
        let jail = jail_from(&root);
        assert!(jail.check_write(&link.join("escaped.txt")).is_err());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
