use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{ChildStdin, ChildStdout, Command},
    time::timeout,
};

fn executable() -> Result<PathBuf, String> {
    let candidates = [
        "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
        "/Applications/Codex.app/Contents/Resources/codex",
        "/opt/homebrew/bin/codex",
        "/usr/local/bin/codex",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|p| p.join("codex"))
                .find(|p| p.is_absolute() && p.is_file())
        })
        .ok_or_else(|| "codex_missing".into())
}

async fn write(input: &mut ChildStdin, message: Value) -> Result<(), String> {
    input
        .write_all(format!("{message}\n").as_bytes())
        .await
        .map_err(|_| "codex_io".to_owned())
}

async fn response(lines: &mut Lines<BufReader<ChildStdout>>, id: u64) -> Result<Value, String> {
    while let Some(line) = lines.next_line().await.map_err(|_| "codex_io")? {
        let data: Value = serde_json::from_str(&line).map_err(|_| "codex_format")?;
        if data["id"].as_u64() == Some(id) {
            if let Some(error) = data["error"]["message"].as_str() {
                return Err(error.into());
            }
            return data
                .get("result")
                .cloned()
                .ok_or_else(|| "codex_format".into());
        }
    }
    Err("codex_io".into())
}

async fn request(method: &str, params: Value) -> Result<Value, String> {
    let mut child = Command::new(executable()?)
        .arg("app-server")
        .current_dir(Path::new("/"))
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut input = child.stdin.take().ok_or("codex_io")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("codex_io")?).lines();
    let result = timeout(Duration::from_secs(45), async {
        write(&mut input, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"memtrans","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})).await?;
        response(&mut lines, 1).await?;
        write(&mut input, json!({"method":"initialized"})).await?;
        write(&mut input, json!({"id":2,"method":method,"params":params})).await?;
        response(&mut lines, 2).await
    }).await.map_err(|_| "codex_timeout");
    drop(input);
    let _ = child.kill().await;
    result?
}

/// Vibe Kanban keeps one git worktree per task under either a temporary
/// `vibe-kanban/worktrees/<name>` tree or a persistent
/// `.vibe-kanban-workspaces/<name>` tree. The directory name is the worktree
/// identifier; the last path segment is the repository name.
pub fn worktree_name(path: &str) -> Option<String> {
    let parts = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        let root = *part == ".vibe-kanban-workspaces"
            || (*part == "worktrees" && index > 0 && parts[index - 1] == "vibe-kanban");
        if root {
            return parts.get(index + 1).map(|name| (*name).to_owned());
        }
    }
    None
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    #[serde(default)]
    pub project_exists: bool,
    pub worktree: Option<String>,
    pub updated_at: i64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub sessions: Vec<Session>,
    pub next_cursor: Option<String>,
}

pub async fn list(cursor: Option<String>) -> Result<Page, String> {
    let data = request("thread/list", json!({"cursor":cursor,"limit":100,"sortKey":"updated_at","modelProviders":[],"currentAccountOnly":true})).await?;
    let rows = data["data"].as_array().ok_or("codex_format")?;
    let mut sessions = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row["id"].as_str().ok_or("codex_format")?;
        let title = row["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or(row["preview"].as_str())
            .unwrap_or(id);
        let project = row["cwd"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        sessions.push(Session {
            id: id.into(),
            title: title
                .lines()
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(id)
                .chars()
                .take(180)
                .collect(),
            project_exists: project
                .as_deref()
                .is_some_and(|path| Path::new(path).is_dir()),
            worktree: project.as_deref().and_then(worktree_name),
            project,
            updated_at: row["updatedAt"].as_i64().ok_or("codex_format")?,
        });
    }
    Ok(Page {
        sessions,
        next_cursor: data["nextCursor"].as_str().map(str::to_owned),
    })
}

pub async fn read(id: &str) -> Result<Value, String> {
    let data = request("thread/read", json!({"threadId":id,"includeTurns":true})).await?;
    if data["thread"]["id"].as_str() != Some(id) {
        return Err("codex_format".into());
    }
    Ok(data["thread"].clone())
}

#[cfg(test)]
mod tests {
    use super::worktree_name;

    #[test]
    fn recognizes_vibe_kanban_worktree_roots() {
        assert_eq!(
            worktree_name("/Users/u/Code/.vibe-kanban-workspaces/1111-/repo").as_deref(),
            Some("1111-")
        );
        assert_eq!(
            worktree_name(
                "/tmp/vibe-kanban/worktrees/2222-header/sample-report"
            )
            .as_deref(),
            Some("2222-header")
        );
        assert_eq!(worktree_name("/Users/u/Code/Memtrans"), None);
        assert_eq!(worktree_name("/Users/u/Code/vibe-kanban/worktrees"), None);
        assert_eq!(worktree_name("/Users/u/Code/.vibe-kanban-workspaces"), None);
    }
}

pub async fn projects() -> Result<Vec<crate::harness::Project>, String> {
    let mut projects = Vec::new();
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    loop {
        let page = request("project/list", json!({"cursor":cursor,"limit":100})).await?;
        for row in page["data"].as_array().ok_or("codex_format")? {
            let id = row["id"].as_str().ok_or("codex_format")?;
            for root in row["roots"].as_array().ok_or("codex_format")? {
                let path = root["path"].as_str().ok_or("codex_format")?;
                if Path::new(path).is_dir() {
                    projects.push(crate::harness::Project {
                        id: id.into(),
                        path: path.into(),
                        title: row["name"].as_str().unwrap_or(path).into(),
                    });
                }
            }
        }
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        match &cursor {
            None => return Ok(projects),
            Some(next) if !seen.insert(next.clone()) => return Err("codex_format".into()),
            _ => {}
        }
    }
}

pub async fn resolve_project(path: &str, id: &str) -> Result<(), String> {
    if projects()
        .await?
        .iter()
        .any(|p| p.id == id && p.path == path)
    {
        Ok(())
    } else {
        Err("codex_project_missing".into())
    }
}

fn contains_handoff(thread: &Value, body: &str) -> bool {
    thread["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|turn| turn["items"].as_array().into_iter().flatten())
        .any(|item| {
            item["type"] == "userMessage"
                && item["content"].as_array().is_some_and(|parts| {
                    parts.len() == 1
                        && parts[0]["type"] == "text"
                        && parts[0]["text"].as_str() == Some(body)
                })
        })
}

// The source rollout is private to Session Take. App Server forks it into its own
// storage and indexes the completed user turn without starting an agent.
async fn fork_handoff(
    record: &crate::records::Record,
    dir: &Path,
    body: &str,
) -> Result<String, String> {
    use std::io::Write;
    let seed = record
        .id
        .strip_prefix("session-")
        .ok_or("operation_missing")?;
    let now = chrono::Utc::now();
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let turn = uuid::Uuid::new_v4().to_string();
    let rows = [
        (
            "session_meta",
            json!({"id":seed,"timestamp":stamp,"cwd":record.cwd,"originator":"memtrans","cli_version":env!("CARGO_PKG_VERSION"),"source":"cli"}),
        ),
        ("event_msg", json!({"type":"task_started","turn_id":turn})),
        (
            "response_item",
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":body}]}),
        ),
        (
            "event_msg",
            json!({"type":"user_message","message":body,"images":[],"local_images":[],"text_elements":[]}),
        ),
        (
            "event_msg",
            json!({"type":"task_complete","turn_id":turn,"last_agent_message":null}),
        ),
    ];
    let path = dir.join(format!("{}.jsonl", record.id));
    let result = async {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        for (kind, payload) in rows {
            writeln!(
                file,
                "{}",
                json!({"timestamp":stamp,"type":kind,"payload":payload})
            )
            .map_err(|e| e.to_string())?;
        }
        drop(file);
        let reply = request(
            "thread/fork",
            json!({"threadId":seed,"path":path,"cwd":record.cwd}),
        )
        .await?;
        reply["thread"]["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "codex_format".into())
    }
    .await;
    let _ = std::fs::remove_file(path);
    result
}

pub async fn find_or_create(record: &crate::records::Record, dir: &Path) -> Result<String, String> {
    let body = record.body.as_deref().ok_or("operation_missing")?;
    // Before the title is set, Codex indexes the first user message as the title.
    // This handles a crash between the native fork and saving its returned ID.
    let marker = body.lines().next().ok_or("operation_missing")?;
    let page = request(
        "thread/list",
        json!({"searchTerm":marker,"currentAccountOnly":true,"modelProviders":[],"limit":100}),
    )
    .await?;
    for row in page["data"].as_array().ok_or("codex_format")? {
        let id = row["id"].as_str().ok_or("codex_format")?;
        let thread = read(id).await?;
        if thread["forkedFromId"].as_str() == record.id.strip_prefix("session-")
            && contains_handoff(&thread, body)
        {
            let _ = std::fs::remove_file(dir.join(format!("{}.jsonl", record.id)));
            return Ok(id.into());
        }
    }
    fork_handoff(record, dir, body).await
}

pub async fn finish_delivery(id: &str, record: &crate::records::Record) -> Result<(), String> {
    let project_id = record
        .project_id
        .as_deref()
        .ok_or("codex_project_missing")?;
    request(
        "thread/metadata/update",
        json!({"threadId":id,"projectId":project_id}),
    )
    .await?;
    request(
        "thread/name/set",
        json!({"threadId":id,"name":record.title}),
    )
    .await?;
    let thread = read(id).await?;
    if thread["projectId"] != project_id
        || !contains_handoff(&thread, record.body.as_deref().ok_or("operation_missing")?)
    {
        return Err("codex_delivery_failed".into());
    }
    Ok(())
}

// Forked handoffs contain one frozen user turn. A later user turn means
// either the desktop consumed our submission or the user continued manually.
fn continued(thread: &Value) -> Result<bool, String> {
    let turns = thread["turns"].as_array().ok_or("codex_format")?;
    Ok(turns.iter().skip(1).any(|turn| {
        turn["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["type"] == "userMessage"))
    }))
}

pub async fn enqueue_continuation(id: &str, operation: &str) -> Result<(), String> {
    let client_id = operation
        .strip_prefix("session-")
        .ok_or("operation_missing")?;
    let mut cursor = None::<String>;
    loop {
        let page = request("thread/queue/list", json!({"threadId":id,"cursor":cursor})).await?;
        if page["data"]
            .as_array()
            .ok_or("codex_format")?
            .iter()
            .any(|entry| entry["clientUserMessageId"] == client_id)
        {
            return Ok(());
        }
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    if continued(&read(id).await?)? {
        return Ok(());
    }
    let result = request(
        "thread/queue/add",
        json!({
            "threadId":id,"clientUserMessageId":client_id,
            "input":[{"type":"text","text":"请根据上一条接力消息中的任务和追加内容继续。"}]
        }),
    )
    .await?;
    if result["queuedSubmission"]["clientUserMessageId"] != client_id {
        return Err("codex_queue_failed".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub async fn open_and_run(id: &str) -> Result<(), String> {
    let thread = uuid::Uuid::parse_str(id).map_err(|_| "codex_format")?;
    let status = Command::new("/usr/bin/open")
        .args([
            "-b",
            "com.openai.codex",
            &format!("codex://threads/{thread}"),
            &format!("codex://threads/wake/{thread}"),
        ])
        .status()
        .await
        .map_err(|_| "codex_open_failed")?;
    if !status.success() {
        return Err("codex_open_failed".into());
    }

    // Launch Services acknowledges URL delivery before Codex resumes the turn.
    timeout(Duration::from_secs(25), async {
        loop {
            if continued(&read(id).await?)? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .map_err(|_| "codex_start_pending")?
}

#[cfg(not(target_os = "macos"))]
pub async fn open_and_run(_: &str) -> Result<(), String> {
    Err("mac_only".into())
}
