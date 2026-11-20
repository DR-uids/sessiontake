use crate::records::Record;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

const CLI: &str = "/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh";

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Project {
    pub id: String,
    pub path: String,
    pub title: String,
}

fn project_title(path: &str, row: &Value) -> String {
    row["title"]
        .as_str()
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.rsplit('/')
                .find(|segment| !segment.is_empty())
                .unwrap_or(path)
                .to_owned()
        })
}

pub fn parse_projects(raw: &str) -> Result<Vec<Project>, String> {
    let data: Value = serde_json::from_str(raw).map_err(|_| "harness_projects_failed")?;
    let table = data["tables"]["workspaces"]
        .as_object()
        .ok_or("harness_projects_failed")?;
    let mut used = HashSet::new();
    let mut projects = Vec::new();
    if let Some(order) = data["global"]["workspaceIds"].as_array() {
        for id in order.iter().filter_map(Value::as_str) {
            if !used.insert(id.to_owned()) {
                continue;
            }
            if let Some(row) = table.get(id) {
                if let Some(path) = row["path"].as_str().filter(|path| !path.is_empty()) {
                    projects.push(Project {
                        id: id.to_owned(),
                        path: path.to_owned(),
                        title: project_title(path, row),
                    });
                }
            }
        }
    }
    let mut remaining = table
        .iter()
        .filter(|(id, _)| !used.contains(*id))
        .filter_map(|(id, row)| {
            row["path"].as_str().map(|path| Project {
                id: id.to_owned(),
                path: path.to_owned(),
                title: project_title(path, row),
            })
        })
        .collect::<Vec<_>>();
    remaining.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.path.cmp(&b.path)));
    projects.extend(remaining);
    Ok(projects)
}

fn home() -> Result<PathBuf, String> {
    std::env::var("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".dsh")))
        .map_err(|_| "harness_projects_failed".to_owned())
}

/// Reads the project list of the running Harness installation. The registry is
/// a read-only, atomically published JSON document; Session Take never writes it.
pub fn projects() -> Result<Vec<Project>, String> {
    let file = home()?.join("storages").join("workspace.json");
    if !file.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(file).map_err(|_| "harness_projects_failed")?;
    parse_projects(&raw)
}

pub fn resolve_project(path: &str) -> Result<Project, String> {
    let canonical = std::fs::canonicalize(path).map_err(|_| "harness_project_dir")?;
    projects()?
        .into_iter()
        .find(|project| Path::new(&project.path) == canonical)
        .ok_or_else(|| "harness_project_missing".into())
}

pub async fn list(resources: &Path) -> Result<crate::codex::Page, String> {
    let result = call(resources, json!({"action":"list"})).await?;
    let mut sessions: Vec<crate::codex::Session> =
        serde_json::from_value(result["sessions"].clone()).map_err(|_| "harness_format")?;
    for row in &mut sessions {
        row.project_exists = row
            .project
            .as_deref()
            .is_some_and(|path| Path::new(path).is_dir());
        row.worktree = row.project.as_deref().and_then(crate::codex::worktree_name);
    }
    Ok(crate::codex::Page {
        sessions,
        next_cursor: None,
    })
}

pub async fn read(id: &str, resources: &Path) -> Result<Value, String> {
    let result = call(resources, json!({"action":"read","id":id})).await?;
    if result["thread"]["id"].as_str() != Some(id) {
        return Err("harness_format".into());
    }
    Ok(result["thread"].clone())
}

pub async fn deliver(record: &Record, resources: &Path) -> Result<(), String> {
    let body = record.body.as_deref().ok_or("operation_missing")?;
    let result = call(
        resources,
        json!({"id":record.id,"body":body,"title":record.title,"cwd":record.cwd}),
    )
    .await?;
    if result["delivered"].as_str() != Some(&record.id) {
        return Err("harness_io".into());
    }
    // A Session is only usable from the sidebar when it is a member of the
    // selected Workspace; writing the log alone leaves it in "未分组".
    if result["workspace"]
        .as_str()
        .filter(|id| !id.is_empty())
        .is_none()
    {
        return Err("harness_workspace_attach".into());
    }
    Ok(())
}

async fn call(resources: &Path, request: Value) -> Result<Value, String> {
    if !Path::new(CLI).is_file() {
        return Err("harness_missing".into());
    }
    let mut child = Command::new(CLI)
        .args(["headless", "--patch"])
        .arg(resources.join("handoff.yml"))
        .arg("-")
        .current_dir(resources)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut input = child.stdin.take().ok_or("harness_io")?;
    let output = timeout(Duration::from_secs(60), async move {
        input.write_all(request.to_string().as_bytes()).await?;
        drop(input);
        child.wait_with_output().await
    })
    .await
    .map_err(|_| "harness_timeout")?
    .map_err(|e| e.to_string())?;
    for line in output.stdout.split(|&b| b == b'\n').rev() {
        if let Ok(reply) = serde_json::from_slice::<Value>(line) {
            if output.status.success() && reply.is_object() && reply.get("error").is_none() {
                return Ok(reply);
            }
            if let Some(error) = reply["error"].as_str() {
                return Err(error.to_owned());
            }
        }
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .find(|s| s.contains("Error:") || s.starts_with("dsh:"));
    Err(reason.unwrap_or("harness_io").chars().take(400).collect())
}

/// Whether the desktop app is running before Session Take opens it. It caches the
/// Workspace registry at startup, so a newly attached Session joins the
/// sidebar only after the next launch.
#[cfg(target_os = "macos")]
pub fn running() -> bool {
    extern "C" {
        fn memtrans_harness_running() -> i32;
    }
    unsafe { memtrans_harness_running() != 0 }
}
#[cfg(not(target_os = "macos"))]
pub fn running() -> bool {
    false
}

#[cfg(target_os = "macos")]
pub async fn open(record: &Record) -> Result<(), String> {
    use std::ffi::CString;
    extern "C" {
        fn memtrans_locate(title: *const std::ffi::c_char, id: *const std::ffi::c_char) -> i32;
    }
    let status = Command::new("/usr/bin/open")
        .args(["-b", "com.deepseek.dsh", "dsh://open"])
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("open_failed".into());
    }
    let title = CString::new(record.title.as_str()).map_err(|_| "locate_failed")?;
    let id = CString::new(record.id.as_str()).map_err(|_| "locate_failed")?;
    let code = tauri::async_runtime::spawn_blocking(move || unsafe {
        memtrans_locate(title.as_ptr(), id.as_ptr())
    })
    .await
    .map_err(|_| "locate_failed")?;
    match code {
        0 => Ok(()),
        1 => Err("accessibility".into()),
        _ => Err("locate_failed".into()),
    }
}
#[cfg(not(target_os = "macos"))]
pub async fn open(_: &Record) -> Result<(), String> {
    Err("mac_only".into())
}

#[cfg(test)]
mod tests {
    use super::parse_projects;

    #[test]
    fn projects_follow_the_stored_order_and_fall_back_to_the_directory_name() {
        let raw = r#"{
            "unit": {"name": "workspace", "version": 2},
            "global": {"workspaceIds": ["b", "missing", "a"]},
            "tables": {"workspaces": {
                "a": {"path": "/Users/u/Code/Memtrans", "title": "Memtrans"},
                "b": {"path": "/Users/u/Code/Notebook"},
                "c": {"path": "/Users/u/Code/Extra", "title": "Extra"}
            }}
        }"#;
        let projects = parse_projects(raw).unwrap();
        let rows = projects
            .iter()
            .map(|project| (project.id.as_str(), project.title.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(rows, [("b", "Notebook"), ("a", "Memtrans"), ("c", "Extra")]);
    }
}
