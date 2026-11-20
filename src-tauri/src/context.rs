use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub struct Context {
    pub text: String,
    pub cwd: String,
    pub version: String,
    pub title: String,
}

pub fn prepare(
    thread: &Value,
    additional: &str,
    target_cwd: Option<&str>,
) -> Result<Context, String> {
    let turns = thread["turns"].as_array().ok_or("codex_format")?;
    let mut users = Vec::new();
    let mut progress = None;
    let mut plan = None;
    for turn in turns {
        for item in turn["items"].as_array().ok_or("codex_format")? {
            match item["type"].as_str() {
                Some("userMessage") => {
                    let text = item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|part| part["type"] == "text")
                        .filter_map(|part| part["text"].as_str())
                        .filter(|s| {
                            !(s.starts_with("<environment_context>")
                                && s.trim_end().ends_with("</environment_context>"))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.trim().is_empty() {
                        users.push(text)
                    }
                }
                Some("agentMessage") => {
                    if let Some(s) = item["text"].as_str().filter(|s| !s.trim().is_empty()) {
                        progress = Some(s)
                    }
                }
                Some("plan") => {
                    if let Some(s) = item["text"].as_str().filter(|s| !s.trim().is_empty()) {
                        plan = Some(s)
                    }
                }
                _ => {}
            }
        }
    }
    let goal = users.first().ok_or("no_text")?;
    let source_cwd = thread["cwd"].as_str().filter(|cwd| !cwd.is_empty());
    let mut text = String::new();
    if let Some(source) = source_cwd {
        text.push_str(&format!("来源工作区\n{source}\n\n"));
    }
    text.push_str(&format!(
        "任务目标\n{goal}\n\n已有进展\n{}\n\n待办（来源计划）\n{}",
        progress.unwrap_or("来源未记录"),
        plan.unwrap_or("来源未记录")
    ));
    if users.len() > 1 {
        text.push_str(&format!("\n\n最近一次用户要求\n{}", users.last().unwrap()));
    }
    text.push_str("\n\n追加内容\n");
    text.push_str(additional);
    if text.len() > 256 * 1024 {
        return Err("too_long".into());
    }
    let title = thread["name"]
        .as_str()
        .unwrap_or(goal)
        .lines()
        .next()
        .unwrap_or("Codex")
        .chars()
        .take(12)
        .collect();
    let cwd = target_cwd
        .map(str::to_owned)
        .or_else(|| source_cwd.map(str::to_owned))
        .or_else(|| std::env::var("HOME").ok())
        .ok_or("project_missing")?;
    let version = digest(
        &serde_json::to_vec(&(thread["updatedAt"].clone(), turns)).map_err(|_| "codex_format")?,
    );
    Ok(Context {
        text,
        cwd,
        version,
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_conversation_and_plan_enter_the_handoff() {
        let thread = json!({"cwd":"/project","updatedAt":56,"turns":[{"items":[
            {"type":"userMessage","content":[{"type":"text","text":"Goal"}]},
            {"type":"agentMessage","text":"Built the form"},
            {"type":"plan","text":"Check recovery"},
            {"type":"commandExecution","aggregatedOutput":"private terminal output"},
            {"type":"reasoning","text":"private reasoning"},
            {"type":"userMessage","content":[{"type":"text","text":"Also add keyboard support"}]}
        ]}]});
        let ctx = prepare(&thread, "  keep\nspacing  ", None).unwrap();
        for needed in [
            "Goal",
            "Built the form",
            "Check recovery",
            "Also add keyboard support",
        ] {
            assert!(ctx.text.contains(needed))
        }
        assert!(ctx.text.contains("来源工作区\n/project\n"));
        assert!(ctx.text.ends_with("  keep\nspacing  "));
        assert!(!ctx.text.contains("private"));
        assert_eq!(ctx.cwd, "/project");
    }

    #[test]
    fn target_project_becomes_the_session_directory_while_the_source_stays_in_the_body() {
        let thread = json!({"cwd":"/tmp/worktrees/2222-header/repo","turns":[{"items":[
            {"type":"userMessage","content":[{"type":"text","text":"Goal"}]}
        ]}]});
        let ctx = prepare(&thread, "", Some("/Users/u/Code/repo")).unwrap();
        assert_eq!(ctx.cwd, "/Users/u/Code/repo");
        assert!(ctx
            .text
            .starts_with("来源工作区\n/tmp/worktrees/2222-header/repo\n"));
    }
}
