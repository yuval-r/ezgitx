mod common;

use common::*;

/// core <- lib <- app, each default_cmd appends its name to a shared log so
/// execution order is observable.
fn chain(f: &Fixture, core_cmd: &str) {
    f.repo("core");
    f.repo("lib");
    f.repo("app");
    f.config(&format!(
        "version: 1\n\
         groups:\n\
         \x20 g:\n\
         \x20   - path: ./core\n\
         \x20     default_cmd: \"{core_cmd}\"\n\
         \x20   - path: ./lib\n\
         \x20     default_cmd: \"echo lib >> ../build.log\"\n\
         \x20     depends_on: [\"core\"]\n\
         \x20   - path: ./app\n\
         \x20     default_cmd: \"echo app >> ../build.log\"\n\
         \x20     depends_on: [\"lib\"]\n"
    ));
}

fn build_log(f: &Fixture) -> Vec<String> {
    std::fs::read_to_string(f.root().join("build.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn with_deps_builds_stale_upstreams_in_order() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");

    let assert = f
        .ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 3);
    assert_eq!(summary(&lines)["passed"], 3);
    assert_eq!(build_log(&f), ["core", "lib", "app"]);
}

#[test]
fn fresh_upstreams_are_skipped() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");

    f.ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);
    std::fs::remove_file(f.root().join("build.log")).unwrap();

    // Everything fresh: only the target runs.
    let assert = f
        .ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 1);
    assert_eq!(build_log(&f), ["app"]);

    // New commit in core: core is stale (own head), and lib is stale too
    // because it was built against the old core (manifest drift). app rebuilds
    // on top of both.
    f.commit(&f.root().join("core"), "change.txt", "x");
    std::fs::remove_file(f.root().join("build.log")).unwrap();
    let assert = f
        .ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 3);
    assert_eq!(build_log(&f), ["core", "lib", "app"]);
}

#[test]
fn upstream_failure_skips_dependents() {
    let f = Fixture::new();
    chain(&f, "false");

    let assert = f
        .ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "core")["exit_code"], 1);
    let lib = line_for(&lines, "lib");
    assert!(lib["exit_code"].is_null());
    assert_eq!(lib["error"]["code"], "upstream_failed");
    assert!(lib["error"]["message"].as_str().unwrap().contains("core"));
    let app = line_for(&lines, "app");
    assert_eq!(app["error"]["code"], "upstream_failed");
    assert_eq!(summary(&lines)["failed"], 3);
    // Nothing downstream actually ran.
    assert_eq!(build_log(&f), Vec::<String>::new());
}

#[test]
fn without_with_deps_staleness_changes_nothing() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");
    // core/lib are stale, but a plain run only executes the target (PRD §9.4).
    let assert = f.ezgitx().args(["run", "--repo", "app"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 1);
    assert_eq!(build_log(&f), ["app"]);
}

#[test]
fn records_dep_manifest() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");

    // First build: nothing is fresh, so the whole chain builds and records.
    f.ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);

    let read_state = |name: &str| -> serde_json::Value {
        let path = f.root().join(format!(".ezgitx/state/{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let core = read_state("core");
    let lib = read_state("lib");
    let app = read_state("app");

    // Each repo records the exact head of every transitive upstream it built
    // against; a repo with no upstreams records an empty map.
    assert_eq!(core["deps"], serde_json::json!({}));
    assert_eq!(lib["deps"]["core"], core["head"]);
    assert_eq!(app["deps"]["core"], core["head"]);
    assert_eq!(app["deps"]["lib"], lib["head"]);
    assert_eq!(app["deps"].as_object().unwrap().len(), 2);
    assert_eq!(lib["deps"].as_object().unwrap().len(), 1);
}

#[test]
fn explicit_cmd_applies_to_targets_not_upstreams() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");
    let assert = f
        .ezgitx()
        .args([
            "run",
            "--repo",
            "app",
            "--with-deps",
            "echo custom >> ../build.log",
        ])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 3);
    // Upstreams ran their default_cmd; the target ran the explicit command.
    assert_eq!(build_log(&f), ["core", "lib", "custom"]);
}

#[test]
fn with_dependents_cascades_downstream() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");
    f.ezgitx()
        .args(["run", "--all", "--with-deps"])
        .assert()
        .code(0);
    std::fs::remove_file(f.root().join("build.log")).unwrap();

    // Change core, then push the change forward to everything on top of it.
    f.commit(&f.root().join("core"), "c.txt", "x");
    let assert = f
        .ezgitx()
        .args(["run", "--repo", "core", "--with-dependents"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 3);
    assert_eq!(build_log(&f), ["core", "lib", "app"]);
}

#[test]
fn with_dependents_skips_fresh_downstream() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");
    f.ezgitx()
        .args(["run", "--all", "--with-deps"])
        .assert()
        .code(0);
    std::fs::remove_file(f.root().join("build.log")).unwrap();

    // Nothing changed: only the target runs (downstreams are fresh against it).
    let assert = f
        .ezgitx()
        .args(["run", "--repo", "core", "--with-dependents"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 1);
    assert_eq!(build_log(&f), ["core"]);
}

#[test]
fn with_dependents_explicit_cmd_hits_target_only() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");
    f.ezgitx()
        .args(["run", "--all", "--with-deps"])
        .assert()
        .code(0);
    std::fs::remove_file(f.root().join("build.log")).unwrap();

    f.commit(&f.root().join("core"), "c.txt", "x");
    let assert = f
        .ezgitx()
        .args([
            "run",
            "--repo",
            "core",
            "--with-dependents",
            "echo custom >> ../build.log",
        ])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 3);
    // Target ran the explicit command; dependents ran their default_cmd.
    assert_eq!(build_log(&f), ["custom", "lib", "app"]);
}

#[test]
fn with_deps_and_with_dependents_compose() {
    let f = Fixture::new();
    chain(&f, "echo core >> ../build.log");
    f.ezgitx()
        .args(["run", "--all", "--with-deps"])
        .assert()
        .code(0);
    std::fs::remove_file(f.root().join("build.log")).unwrap();

    // Change core, then target lib expanding BOTH directions: --with-deps pulls
    // in its upstream (core), --with-dependents pulls in its downstream (app).
    // All three are stale, so all rebuild in dependency order.
    f.commit(&f.root().join("core"), "c.txt", "x");
    let assert = f
        .ezgitx()
        .args(["run", "--repo", "lib", "--with-deps", "--with-dependents"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 3);
    assert_eq!(build_log(&f), ["core", "lib", "app"]);
}
