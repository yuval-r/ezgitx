use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{self, CONFIG_FILE};
use crate::errors::{ErrorCode, ErrorInfo};
use crate::graph;

#[derive(Clone, Debug)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub default_cmd: Option<String>,
    pub check_cmd: Option<String>,
    pub depends_on: Vec<String>,
}

impl Repo {
    /// The command for check/verify mode (`check-impact --check`, `verify`):
    /// `check_cmd`, falling back to `default_cmd`; `no_default_cmd` when neither
    /// is set. (`run` resolves commands differently: default_cmd, or the
    /// explicit command for targets, so it does not use this.)
    pub fn check_command(&self) -> Result<String, ErrorInfo> {
        self.check_cmd
            .clone()
            .or_else(|| self.default_cmd.clone())
            .ok_or_else(|| {
                ErrorInfo::new(
                    ErrorCode::NoDefaultCmd,
                    format!("repo {:?} has neither check_cmd nor default_cmd", self.name),
                )
            })
    }
}

#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    /// Keyed by directory name; BTreeMap gives deterministic ordering.
    pub repos: BTreeMap<String, Repo>,
    pub groups: BTreeMap<String, Vec<String>>,
}

/// Targeting flags shared by status/pull/run (PRD §4.3).
#[derive(Debug, Default)]
pub struct Targeting {
    pub all: bool,
    pub groups: Vec<String>,
    pub repos: Vec<String>,
}

/// Walk upward from `start` until a directory containing `.ezgitx.yml` is found.
pub fn discover_root(start: &Path) -> Result<PathBuf, ErrorInfo> {
    let mut dir = start;
    loop {
        if dir.join(CONFIG_FILE).is_file() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => {
                return Err(ErrorInfo::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "no {CONFIG_FILE} found in {} or any parent",
                        start.display()
                    ),
                ));
            }
        }
    }
}

pub fn load(start: &Path) -> Result<Workspace, ErrorInfo> {
    let root = discover_root(start)?;
    let text = std::fs::read_to_string(root.join(CONFIG_FILE)).map_err(|e| {
        ErrorInfo::new(
            ErrorCode::ConfigInvalid,
            format!("cannot read {CONFIG_FILE}: {e}"),
        )
    })?;
    let cfg = config::parse(&text).map_err(|e| {
        ErrorInfo::new(
            ErrorCode::ConfigInvalid,
            format!("invalid {CONFIG_FILE}: {e}"),
        )
    })?;

    // Merged at the Option level so "not specified" stays distinct from
    // explicit values (e.g. `depends_on: []`) until every group is seen —
    // collapsing early makes conflict detection depend on group order.
    struct Pending {
        path: PathBuf,
        default_cmd: Option<String>,
        check_cmd: Option<String>,
        depends_on: Option<Vec<String>>,
    }
    let mut pending: BTreeMap<String, Pending> = BTreeMap::new();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (group_name, entries) in &cfg.groups {
        let members = groups.entry(group_name.clone()).or_default();
        for entry in entries {
            let path = normalize(&root.join(&entry.path));
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
                .ok_or_else(|| {
                    ErrorInfo::new(
                        ErrorCode::ConfigInvalid,
                        format!("repo path {:?} has no directory name", entry.path),
                    )
                })?;
            if let Some(existing) = pending.get_mut(&name) {
                if existing.path != path {
                    return Err(ErrorInfo::new(
                        ErrorCode::ConfigInvalid,
                        format!(
                            "repo name {name:?} maps to both {} and {}",
                            existing.path.display(),
                            path.display()
                        ),
                    ));
                }
                // Same repo listed in another group: entries MERGE — fields
                // fill in from whichever entry provides them. First-wins
                // would resolve by group iteration order (alphabetical, not
                // file order), silently dropping commands. Conflicting
                // values fail loudly instead (PRD §4.1).
                merge_field(
                    &mut existing.default_cmd,
                    &entry.default_cmd,
                    "default_cmd",
                    &name,
                )?;
                merge_field(
                    &mut existing.check_cmd,
                    &entry.check_cmd,
                    "check_cmd",
                    &name,
                )?;
                merge_field(
                    &mut existing.depends_on,
                    &entry.depends_on,
                    "depends_on",
                    &name,
                )?;
            } else {
                pending.insert(
                    name.clone(),
                    Pending {
                        path,
                        default_cmd: entry.default_cmd.clone(),
                        check_cmd: entry.check_cmd.clone(),
                        depends_on: entry.depends_on.clone(),
                    },
                );
            }
            if !members.contains(&name) {
                members.push(name);
            }
        }
    }

    let repos: BTreeMap<String, Repo> = pending
        .into_iter()
        .map(|(name, p)| {
            (
                name.clone(),
                Repo {
                    name,
                    path: p.path,
                    default_cmd: p.default_cmd,
                    check_cmd: p.check_cmd,
                    depends_on: p.depends_on.unwrap_or_default(),
                },
            )
        })
        .collect();

    for repo in repos.values() {
        for dep in &repo.depends_on {
            if !repos.contains_key(dep) {
                return Err(ErrorInfo::new(
                    ErrorCode::ConfigInvalid,
                    format!("repo {:?} depends_on unknown repo {dep:?}", repo.name),
                ));
            }
        }
    }

    let ws = Workspace {
        root,
        repos,
        groups,
    };
    graph::check_cycles(&ws)?;
    Ok(ws)
}

impl Workspace {
    /// The member repo containing `cwd`, if any.
    pub fn current_repo(&self, cwd: &Path) -> Option<&Repo> {
        let cwd = normalize(cwd);
        self.repos.values().find(|r| cwd.starts_with(&r.path))
    }

    /// Resolve targeting flags to a deterministic, deduplicated repo list
    /// (PRD §4.3). No flags: all repos at the root, the enclosing repo when
    /// inside a member. The `--dirty` filter is applied separately (it needs
    /// git calls).
    pub fn select(&self, t: &Targeting, cwd: &Path) -> Result<Vec<Repo>, ErrorInfo> {
        // BTreeSet dedupes and keeps the result deterministically sorted.
        let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        if t.all {
            names.extend(self.repos.keys().cloned());
        }
        for group in &t.groups {
            let members = self.groups.get(group).ok_or_else(|| {
                ErrorInfo::new(ErrorCode::ConfigInvalid, format!("unknown group {group:?}"))
            })?;
            names.extend(members.iter().cloned());
        }
        for repo in &t.repos {
            if !self.repos.contains_key(repo) {
                return Err(ErrorInfo::new(
                    ErrorCode::ConfigInvalid,
                    format!("unknown repo {repo:?}"),
                ));
            }
            names.insert(repo.clone());
        }

        if !t.all && t.groups.is_empty() && t.repos.is_empty() {
            match self.current_repo(cwd) {
                Some(repo) => {
                    names.insert(repo.name.clone());
                }
                None => names.extend(self.repos.keys().cloned()),
            }
        }

        Ok(names.into_iter().map(|n| self.repos[&n].clone()).collect())
    }
}

/// Merge an optional config field from another group's entry for the same
/// repo: absent stays, missing fills in, identical passes, conflict errors.
fn merge_field<T: PartialEq + Clone + std::fmt::Debug>(
    existing: &mut Option<T>,
    incoming: &Option<T>,
    field: &str,
    repo: &str,
) -> Result<(), ErrorInfo> {
    match (existing.as_ref(), incoming.as_ref()) {
        (_, None) => Ok(()),
        (None, Some(value)) => {
            *existing = Some(value.clone());
            Ok(())
        }
        (Some(a), Some(b)) if a == b => Ok(()),
        (Some(a), Some(b)) => Err(ErrorInfo::new(
            ErrorCode::ConfigInvalid,
            format!("repo {repo:?} has conflicting {field} across groups: {a:?} vs {b:?}"),
        )),
    }
}

/// Lexically resolve `.` and `..` without touching the filesystem (the paths
/// may not exist yet — validation is lazy, per PRD §4.1). Leading `..`
/// components of relative paths are preserved rather than silently dropped.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(
                    out.components().next_back(),
                    None | Some(std::path::Component::ParentDir)
                ) {
                    out.push(comp);
                } else {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_with(yaml: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), yaml).unwrap();
        let ws = load(dir.path()).unwrap();
        (dir, ws)
    }

    const BASIC: &str = "version: 1\ngroups:\n  g1:\n    - path: ./a\n    - path: ./b\n  g2:\n    - path: ./b\n    - path: ./c\n";

    #[test]
    fn dedupes_repo_in_multiple_groups() {
        let (_d, ws) = ws_with(BASIC);
        assert_eq!(ws.repos.len(), 3);
        let all = ws
            .select(
                &Targeting {
                    all: true,
                    ..Default::default()
                },
                &ws.root,
            )
            .unwrap();
        assert_eq!(
            all.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn group_and_repo_union() {
        let (_d, ws) = ws_with(BASIC);
        let t = Targeting {
            groups: vec!["g2".into()],
            repos: vec!["a".into()],
            ..Default::default()
        };
        let sel = ws.select(&t, &ws.root).unwrap();
        assert_eq!(
            sel.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn unknown_group_is_config_error() {
        let (_d, ws) = ws_with(BASIC);
        let t = Targeting {
            groups: vec!["nope".into()],
            ..Default::default()
        };
        assert!(ws.select(&t, &ws.root).is_err());
    }

    #[test]
    fn no_flags_at_root_selects_all() {
        let (_d, ws) = ws_with(BASIC);
        let sel = ws.select(&Targeting::default(), &ws.root).unwrap();
        assert_eq!(sel.len(), 3);
    }

    #[test]
    fn no_flags_inside_repo_selects_that_repo() {
        let (_d, ws) = ws_with(BASIC);
        let inside = ws.root.join("b").join("src");
        let sel = ws.select(&Targeting::default(), &inside).unwrap();
        assert_eq!(
            sel.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["b"]
        );
    }

    #[test]
    fn discovery_walks_upward() {
        let (_d, ws) = ws_with(BASIC);
        let deep = ws.root.join("a/x/y");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(discover_root(&deep).unwrap(), ws.root);
    }

    #[test]
    fn multi_group_fields_merge_regardless_of_group_order() {
        // Dogfooded bug: groups iterate in BTreeMap (alphabetical) order, so
        // a bare membership entry in an alphabetically-earlier group ("aux")
        // silently erased the real entry's commands under first-wins.
        let (_d, ws) = ws_with(
            "version: 1\n\
             groups:\n\
             \x20 aux:\n\
             \x20   - path: ./sdk\n\
             \x20 build:\n\
             \x20   - path: ./sdk\n\
             \x20     default_cmd: \"make\"\n\
             \x20     check_cmd: \"make test\"\n\
             \x20   - path: ./app\n\
             \x20     depends_on: [\"sdk\"]\n\
             \x20 extra:\n\
             \x20   - path: ./app\n",
        );
        let sdk = &ws.repos["sdk"];
        assert_eq!(sdk.default_cmd.as_deref(), Some("make"));
        assert_eq!(sdk.check_cmd.as_deref(), Some("make test"));
        assert_eq!(ws.repos["app"].depends_on, ["sdk"]);
    }

    #[test]
    fn conflicting_fields_across_groups_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "version: 1\n\
             groups:\n\
             \x20 g1:\n\
             \x20   - path: ./a\n\
             \x20     default_cmd: \"make\"\n\
             \x20 g2:\n\
             \x20   - path: ./a\n\
             \x20     default_cmd: \"cargo build\"\n",
        )
        .unwrap();
        let err = load(dir.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        assert!(
            err.message.contains("default_cmd"),
            "message: {}",
            err.message
        );
    }

    #[test]
    fn explicit_empty_depends_on_conflicts_symmetrically() {
        // depends_on: [] is an explicit declaration, not absence. It must
        // conflict with a non-empty list in BOTH group orders — merging
        // silently in one order and erroring in the other is the same
        // order-dependence this change exists to remove.
        for (first_group, second_group) in [("aaa", "zzz"), ("zzz", "aaa")] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(CONFIG_FILE),
                format!(
                    "version: 1\n\
                     groups:\n\
                     \x20 {first_group}:\n\
                     \x20   - path: ./a\n\
                     \x20     depends_on: []\n\
                     \x20   - path: ./sdk\n\
                     \x20 {second_group}:\n\
                     \x20   - path: ./a\n\
                     \x20     depends_on: [\"sdk\"]\n"
                ),
            )
            .unwrap();
            let err = load(dir.path()).unwrap_err();
            assert_eq!(
                err.code,
                ErrorCode::ConfigInvalid,
                "groups {first_group}/{second_group} must conflict"
            );
            assert!(
                err.message.contains("depends_on"),
                "message: {}",
                err.message
            );
        }
    }

    #[test]
    fn identical_duplicate_fields_are_fine() {
        let (_d, ws) = ws_with(
            "version: 1\n\
             groups:\n\
             \x20 g1:\n\
             \x20   - path: ./a\n\
             \x20     default_cmd: \"make\"\n\
             \x20 g2:\n\
             \x20   - path: ./a\n\
             \x20     default_cmd: \"make\"\n",
        );
        assert_eq!(ws.repos["a"].default_cmd.as_deref(), Some("make"));
    }

    #[test]
    fn normalize_preserves_leading_parent_dirs() {
        use std::path::Path;
        assert_eq!(normalize(Path::new("a/../../b")), Path::new("../b"));
        assert_eq!(normalize(Path::new("../../x")), Path::new("../../x"));
        assert_eq!(normalize(Path::new("/a/../../b")), Path::new("/b"));
        assert_eq!(
            normalize(Path::new("/w/./repo/../other")),
            Path::new("/w/other")
        );
    }

    #[test]
    fn name_collision_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            "version: 1\ngroups:\n  g:\n    - path: ./x/app\n    - path: ./y/app\n",
        )
        .unwrap();
        assert!(load(dir.path()).is_err());
    }
}
