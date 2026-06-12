use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::errors::{ErrorCode, ErrorInfo};
use crate::workspace::Workspace;

/// Reject cycles in `depends_on` at config load (PRD §9.2): iterative DFS
/// with three-color marking, reporting the cycle path in the message.
pub fn check_cycles(ws: &Workspace) -> Result<(), ErrorInfo> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        White,
        Gray,
        Black,
    }
    let mut marks: BTreeMap<&str, Mark> =
        ws.repos.keys().map(|k| (k.as_str(), Mark::White)).collect();

    for start in ws.repos.keys() {
        if marks[start.as_str()] != Mark::White {
            continue;
        }
        // Stack of (node, next-dep-index); the gray chain is the stack itself.
        let mut stack: Vec<(&str, usize)> = vec![(start.as_str(), 0)];
        marks.insert(start.as_str(), Mark::Gray);
        while let Some(&(node, idx)) = stack.last() {
            let deps = &ws.repos[node].depends_on;
            if idx < deps.len() {
                stack.last_mut().unwrap().1 += 1;
                let dep = deps[idx].as_str();
                match marks[dep] {
                    Mark::White => {
                        marks.insert(dep, Mark::Gray);
                        stack.push((dep, 0));
                    }
                    Mark::Gray => {
                        let from = stack.iter().position(|(n, _)| *n == dep).unwrap_or(0);
                        let mut cycle: Vec<&str> = stack[from..].iter().map(|(n, _)| *n).collect();
                        cycle.push(dep);
                        return Err(ErrorInfo::new(
                            ErrorCode::DependencyCycle,
                            format!("dependency cycle: {}", cycle.join(" -> ")),
                        ));
                    }
                    Mark::Black => {}
                }
            } else {
                marks.insert(node, Mark::Black);
                stack.pop();
            }
        }
    }
    Ok(())
}

/// All transitive upstream dependencies of `name` (excluding itself).
pub fn transitive_upstreams(ws: &Workspace, name: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    queue.push_back(name);
    while let Some(node) = queue.pop_front() {
        if let Some(repo) = ws.repos.get(node) {
            for dep in &repo.depends_on {
                if seen.insert(dep.clone()) {
                    queue.push_back(dep);
                }
            }
        }
    }
    seen.remove(name);
    seen
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Affected {
    pub repo: String,
    pub depth: usize,
    /// Dependency path from the changed repo (inclusive) up to, but not
    /// including, this repo: depth 1 → ["changed"], depth 2 → ["changed", "mid"].
    pub via: Vec<String>,
}

/// Transitive downstream closure of `name` (PRD §9.5): BFS over reverse
/// `depends_on` edges, shortest-path depth, deterministic order
/// (depth, then name).
pub fn downstream_closure(ws: &Workspace, name: &str) -> Vec<Affected> {
    let mut reverse: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for repo in ws.repos.values() {
        for dep in &repo.depends_on {
            reverse
                .entry(dep.as_str())
                .or_default()
                .push(repo.name.as_str());
        }
    }

    let mut out: Vec<Affected> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    seen.insert(name);
    let mut frontier: Vec<(&str, Vec<String>)> = vec![(name, vec![])];
    let mut depth = 0;
    while !frontier.is_empty() {
        depth += 1;
        let mut next: Vec<(&str, Vec<String>)> = Vec::new();
        for (node, via) in &frontier {
            let mut via_here = via.clone();
            via_here.push(node.to_string());
            if let Some(dependents) = reverse.get(node) {
                for dependent in dependents {
                    if seen.insert(dependent) {
                        out.push(Affected {
                            repo: dependent.to_string(),
                            depth,
                            via: via_here.clone(),
                        });
                        next.push((dependent, via_here.clone()));
                    }
                }
            }
        }
        next.sort_by(|a, b| a.0.cmp(b.0));
        frontier = next;
    }
    out.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.repo.cmp(&b.repo)));
    out
}

/// Topological *waves* over `set` (PRD §3.5): repos within a wave may run in
/// parallel; waves run in dependency order. Edges are the *transitive*
/// upstream relation restricted to `set` — members may depend on each other
/// through repos that are not in the set (e.g. fresh intermediates skipped
/// by `--with-deps`), and that ordering must still hold.
pub fn topo_waves(ws: &Workspace, set: &BTreeSet<String>) -> Vec<Vec<String>> {
    let deps_in_set: BTreeMap<&str, BTreeSet<String>> = set
        .iter()
        .map(|name| {
            let mut ups = transitive_upstreams(ws, name);
            ups.retain(|u| set.contains(u));
            (name.as_str(), ups)
        })
        .collect();

    let mut waves: Vec<Vec<String>> = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while done.len() < set.len() {
        let wave: Vec<String> = set
            .iter()
            .filter(|n| !done.contains(*n))
            .filter(|n| deps_in_set[n.as_str()].iter().all(|d| done.contains(d)))
            .cloned()
            .collect();
        debug_assert!(!wave.is_empty(), "cycle should have been rejected at load");
        done.extend(wave.iter().cloned());
        waves.push(wave);
    }
    waves
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Repo;
    use std::path::PathBuf;

    fn ws(edges: &[(&str, &[&str])]) -> Workspace {
        let repos = edges
            .iter()
            .map(|(name, deps)| {
                (
                    name.to_string(),
                    Repo {
                        name: name.to_string(),
                        path: PathBuf::from(format!("/w/{name}")),
                        default_cmd: None,
                        check_cmd: None,
                        depends_on: deps.iter().map(|d| d.to_string()).collect(),
                    },
                )
            })
            .collect();
        Workspace {
            root: PathBuf::from("/w"),
            repos,
            groups: BTreeMap::new(),
        }
    }

    #[test]
    fn detects_cycle() {
        let w = ws(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
        let err = check_cycles(&w).unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::DependencyCycle);
        assert!(err.message.contains("->"));
    }

    #[test]
    fn accepts_dag() {
        let w = ws(&[("a", &["b", "c"]), ("b", &["c"]), ("c", &[])]);
        assert!(check_cycles(&w).is_ok());
    }

    #[test]
    fn transitive_upstreams_walks_chain() {
        let w = ws(&[("app", &["lib"]), ("lib", &["core"]), ("core", &[])]);
        let ups = transitive_upstreams(&w, "app");
        assert_eq!(ups.into_iter().collect::<Vec<_>>(), ["core", "lib"]);
    }

    #[test]
    fn downstream_closure_depth_and_via() {
        let w = ws(&[
            ("app", &["lib"]),
            ("tool", &["lib"]),
            ("lib", &["core"]),
            ("core", &[]),
        ]);
        let affected = downstream_closure(&w, "core");
        assert_eq!(affected.len(), 3);
        assert_eq!(affected[0].repo, "lib");
        assert_eq!(affected[0].depth, 1);
        assert_eq!(affected[0].via, vec!["core"]);
        assert_eq!(affected[1].repo, "app");
        assert_eq!(affected[1].depth, 2);
        assert_eq!(affected[1].via, vec!["core", "lib"]);
    }

    #[test]
    fn diamond_reaches_each_repo_once() {
        let w = ws(&[
            ("top", &[]),
            ("l", &["top"]),
            ("r", &["top"]),
            ("bot", &["l", "r"]),
        ]);
        let affected = downstream_closure(&w, "top");
        assert_eq!(affected.len(), 3);
        assert_eq!(affected[2].repo, "bot");
        assert_eq!(affected[2].depth, 2);
    }

    #[test]
    fn waves_respect_dependency_order() {
        let w = ws(&[
            ("app", &["lib"]),
            ("tool", &["lib"]),
            ("lib", &["core"]),
            ("core", &[]),
        ]);
        let set: BTreeSet<String> = ["app", "tool", "lib", "core"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let waves = topo_waves(&w, &set);
        assert_eq!(
            waves,
            vec![
                vec!["core".to_string()],
                vec!["lib".to_string()],
                vec!["app".to_string(), "tool".to_string()]
            ]
        );
    }

    #[test]
    fn waves_keep_transitive_order_through_excluded_repos() {
        let w = ws(&[("app", &["lib"]), ("lib", &["core"]), ("core", &[])]);
        let set: BTreeSet<String> = ["app", "core"].iter().map(|s| s.to_string()).collect();
        let waves = topo_waves(&w, &set);
        // "lib" is not in the set, but app still depends on core through it.
        assert_eq!(
            waves,
            vec![vec!["core".to_string()], vec!["app".to_string()]]
        );
    }
}
