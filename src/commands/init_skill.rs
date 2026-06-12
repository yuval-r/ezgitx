use serde::Serialize;

use crate::errors::{EXIT_OK, EXIT_USAGE, ErrorCode, ErrorInfo};
use crate::workspace::Workspace;

const SKILL_TEMPLATE: &str = include_str!("../../assets/SKILL.md");

#[derive(Serialize)]
struct InitSkillLine {
    path: String,
    status: &'static str,
}

/// `ezgitx init-skill` (PRD §5.4): write the agent-facing skill file at the
/// workspace root. Idempotent — re-running overwrites.
pub fn run(ws: &Workspace, human: bool) -> i32 {
    let dir = ws.root.join(".claude").join("skills").join("ezgitx");
    let path = dir.join("SKILL.md");
    let result = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, SKILL_TEMPLATE));
    if let Err(e) = result {
        crate::errors::print_top_level(&ErrorInfo::new(
            ErrorCode::ConfigInvalid,
            format!("cannot write {}: {e}", path.display()),
        ));
        return EXIT_USAGE;
    }
    let line = InitSkillLine {
        path: path.display().to_string(),
        status: "written",
    };
    if human {
        println!("wrote {}", line.path);
    } else {
        println!("{}", serde_json::to_string(&line).unwrap());
    }
    EXIT_OK
}
