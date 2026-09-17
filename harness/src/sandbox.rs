//! Path jail: the hard sandbox boundary; every path resolves canonically inside it (`..` and symlink escapes rejected).
//! Approvals gate *whether/when* a tool runs, never *where*; escapes must be pre-declared (`extra_read`/`extra_write`).

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
    Denied,
}

/// Normalize for comparison: strip Windows verbatim prefix; lowercase on Windows for case-insensitive fs.
fn normalize(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if cfg!(windows) {
        PathBuf::from(text.to_lowercase())
    } else {
        PathBuf::from(text.to_owned())
    }
}

/// Canonicalize a maybe-missing target via the deepest existing ancestor.
/// Suffix components cannot contain symlinks, so the ancestor check stays airtight.
fn canonicalize_target(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let lex = lexical_normalize(path);
            let mut probe: &Path = lex.as_path();
            let mut suffix_rev: Vec<std::ffi::OsString> = Vec::new();
            loop {
                match probe.parent() {
                    Some(parent) => {
                        if let Some(name) = probe.file_name() {
                            suffix_rev.push(name.to_os_string());
                        }
                        probe = parent;
                    }
                    None => bail!("Path has no existing ancestor: {}", path.display()),
                }
                if std::fs::metadata(probe).is_ok() {
                    break;
                }
            }
            let base = std::fs::canonicalize(probe)
                .with_context(|| format!("Cannot resolve {}", probe.display()))?;
            let mut out = base;
            for comp in suffix_rev.iter().rev() {
                out.push(comp);
            }
            Ok(out)
        }
        Err(e) => Err(e).with_context(|| format!("Cannot resolve path {}", path.display())),
    }
}

/// Component-boundary prefix test: `dir` contains `path`. Trailing components
/// are preserved; `H:\a\b` does NOT match a dir `H:\projects\foobar`.
fn starts_with_dir(dir: &Path, path: &Path) -> bool {
    path.strip_prefix(dir).is_ok()
}

/// Collapse `.`/`..` lexically for user-visible paths; safety checks always run on the canonical form.
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

    /// A jail on another root with the same extra scope (used for subagent
    /// git worktrees: the branch checkout inherits the project allowlist).
    pub fn rooted_at(&self, root: &Path) -> Result<Self> {
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("Worktree root does not exist: {}", root.display()))?;
        if !root.is_dir() {
            bail!("Worktree root is not a directory: {}", root.display());
        }
        Ok(Self {
            root: normalize(&root),
            extra_read: self.extra_read.clone(),
            extra_write: self.extra_write.clone(),
        })
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

    /// Anchor tool paths: relative and bare-rooted (`/Assets/...`) resolve inside the jail (models emit root-anchored paths).
    /// Fully-qualified paths and `..` keep exact semantics; the canonical check below still applies so remapping can't escape.
    fn anchored(&self, path: &Path) -> PathBuf {
        let mut comps = path.components().peekable();
        if matches!(comps.peek(), Some(Component::RootDir)) {
            comps.next();
        }
        let rel: PathBuf = comps.collect();
        if rel.is_absolute() {
            lexical_normalize(&rel)
        } else {
            self.root.join(lexical_normalize(&rel))
        }
    }

    pub fn check_read(&self, path: &Path) -> Result<PathBuf> {
        let canonical = canonicalize_target(&self.anchored(path))?;
        match self.scope(&canonical) {
            PathScope::Project | PathScope::ExtraRead | PathScope::ExtraWrite => Ok(canonical),
            PathScope::Denied => bail!(
                "Read denied: '{}' is outside the sandboxed project and not in the read allowlist",
                lexical_normalize(path).display()
            ),
        }
    }

    /// Validate a write target via the deepest existing ancestor (airtight: missing components can't hide symlinks).
    /// Extra-read roots are never writable.
    pub fn check_write(&self, path: &Path) -> Result<PathBuf> {
        let canonical = canonicalize_target(&self.anchored(path))?;
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
    fn bare_rooted_paths_address_the_project_root() {
        // Models habitually emit `/Assets/...` (or `\Assets\...` on Windows)
        // for files inside the project — resolve them inside the jail.
        let root = temp_dir("rooted");
        let jail = jail_from(&root);
        let f = root.join("Assets").join("img.png");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "x").unwrap();
        let rooted = std::path::Path::new("/").join("Assets").join("img.png");
        let ok = jail.check_read(&rooted).unwrap();
        assert_eq!(ok.file_name().unwrap(), "img.png");
        // …but a rooted escape still canonicalizes outside and is denied.
        let escape = std::path::Path::new("/").join("..").join("nope.txt");
        assert!(jail.check_read(&escape).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_parents_resolve_within_jail() {
        let root = temp_dir("missing-parent");
        let jail = jail_from(&root);
        assert!(jail.check_write(&root.join("no").join("such.txt")).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn deep_prefix_lookalike_does_not_match() {
        let root = temp_dir("prefix");
        let jail = jail_from(&root);
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
