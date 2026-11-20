mod editors;
pub use editors::cursor_events;
pub(crate) use editors::{user_prompt, without_reminders};

use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Codex,
    Harness,
    Claude,
    Cursor,
    Workbuddy,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Failure {
    HomeUnavailable,
    Permission,
    Missing,
    InvalidRecord,
    Io,
    Index,
    SourceDestination,
    SaveDialog,
    Clipboard,
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::Permission,
            io::ErrorKind::NotFound => Self::Missing,
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => Self::InvalidRecord,
            _ => Self::Io,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    Codex,
    Harness,
    ClaudeCli,
    ClaudeDesktop,
    Cursor,
    Workbuddy,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub key: String,
    pub id: String,
    pub source: Source,
    pub origin: Origin,
    pub title: String,
    pub project: Option<String>,
    pub project_exists: bool,
    pub repository: Option<String>,
    pub worktree: Option<String>,
    pub worktree_repository: Option<String>,
    pub updated_at: i64,
}

#[derive(Clone)]
pub enum Record {
    File(PathBuf),
    CursorComposer {
        db: PathBuf,
        transcript: Option<PathBuf>,
    },
    CodexProjection(PathBuf),
    ClaudeMissing(Vec<PathBuf>),
}

#[derive(Clone)]
pub struct Indexed {
    pub session: Session,
    pub record: Record,
}

pub struct Roots {
    codex: PathBuf,
    harness: PathBuf,
    claude: PathBuf,
    claude_default: PathBuf,
    claude_desktop: PathBuf,
    workbuddy: PathBuf,
    cursor: PathBuf,
    cursor_user: PathBuf,
}

impl Roots {
    pub fn local() -> Result<Self, Failure> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(Failure::HomeUnavailable)?;
        let default_claude = home.join(".claude");
        Ok(Self {
            codex: configured("CODEX_HOME", home.join(".codex")),
            harness: configured("DSH_HOME", home.join(".dsh")),
            claude: configured("CLAUDE_CONFIG_DIR", default_claude.clone()),
            claude_default: default_claude,
            workbuddy: home.join(".workbuddy"),
            cursor: home.join(".cursor"),
            cursor_user: home.join("Library/Application Support/Cursor/User"),
            claude_desktop: home.join("Library/Application Support/Claude/claude-code-sessions"),
        })
    }

    pub fn all(&self) -> [&Path; 8] {
        [
            &self.codex,
            &self.harness,
            &self.claude,
            &self.claude_default,
            &self.claude_desktop,
            &self.workbuddy,
            &self.cursor,
            &self.cursor_user,
        ]
    }

    fn claude_projects(&self) -> Vec<PathBuf> {
        let mut projects = vec![self.claude.join("projects")];
        if self.claude != self.claude_default {
            projects.push(self.claude_default.join("projects"));
        }
        projects
    }
}

fn configured(name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

pub struct Catalog {
    pub sessions: Vec<Indexed>,
    pub warning: Option<Failure>,
}

impl Catalog {
    pub fn load(roots: &Roots, source: Source) -> Self {
        let mut catalog = Self {
            sessions: Vec::new(),
            warning: None,
        };
        match source {
            Source::Codex => catalog.codex(&roots.codex),
            Source::Harness => catalog.harness(&roots.harness),
            Source::Claude => catalog.claude(roots),
            Source::Cursor => catalog.cursor(roots),
            Source::Workbuddy => catalog.workbuddy(&roots.workbuddy),
        }
        catalog.sessions.sort_by(|a, b| {
            b.session
                .updated_at
                .cmp(&a.session.updated_at)
                .then_with(|| a.session.key.cmp(&b.session.key))
        });
        catalog
    }

    fn files(&mut self, root: &Path, depth: usize) -> Vec<PathBuf> {
        let mut files = Vec::new();
        self.walk(root, depth, &mut files);
        files.sort_unstable();
        files
    }

    fn walk(&mut self, root: &Path, depth: usize, files: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => {
                self.warning.get_or_insert(error.into());
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.warning.get_or_insert(error.into());
                    continue;
                }
            };
            match entry.file_type() {
                Ok(ty) if ty.is_dir() && depth > 0 => self.walk(&entry.path(), depth - 1, files),
                Ok(ty) if ty.is_file() => files.push(entry.path()),
                Ok(_) => (),
                Err(error) => {
                    self.warning.get_or_insert(error.into());
                }
            }
        }
    }

    fn codex(&mut self, root: &Path) {
        let mut names = HashMap::new();
        match json_lines(&root.join("session_index.jsonl")) {
            Ok(lines) => {
                for line in lines {
                    match line {
                        Ok(value) => {
                            if let (Some(id), Some(name)) =
                                (string(&value["id"]), string(&value["thread_name"]))
                            {
                                names.insert(id.to_owned(), name.to_owned());
                            }
                        }
                        Err(error) => {
                            self.warning.get_or_insert(error);
                        }
                    }
                }
            }
            Err(Failure::Missing) => (),
            Err(error) => {
                self.warning.get_or_insert(error);
            }
        }

        let db = newest_db(root, "state_", &mut self.warning);
        let history = newest_db(root, "thread_history_", &mut self.warning);
        let mut rows = db
            .as_deref()
            .map(|path| self.optional_index(codex_index(path)))
            .unwrap_or_default();
        let projected = history
            .as_deref()
            .map(|path| self.optional_index(projected_ids(path)))
            .unwrap_or_default();

        let mut files = self.files(&root.join("sessions"), 3);
        files.extend(self.files(&root.join("archived_sessions"), 3));
        files.retain(|path| extension(path, "jsonl"));
        let mut seen = HashSet::new();
        for file in files {
            let mut info = codex_header(&file);
            if info.skip {
                continue;
            }
            let id = info.id.take().unwrap_or_else(|| rollout_id(&file));
            if !seen.insert(id.clone()) {
                continue;
            }
            let stored = rows.remove(&id).unwrap_or_default();
            if stored.skip {
                continue;
            }
            info.title = names.remove(&id).or(stored.title).or(info.title);
            info.project = stored.project.or(info.project);
            info.updated_at = modified(&file).max(stored.updated_at);
            self.sessions.push(indexed(
                id,
                Source::Codex,
                Origin::Codex,
                info,
                Record::File(file),
            ));
        }
        if let Some(history) = history {
            for (id, mut info) in rows {
                if info.skip || !projected.contains(&id) {
                    continue;
                }
                info.title = names.remove(&id).or(info.title);
                self.sessions.push(indexed(
                    id,
                    Source::Codex,
                    Origin::Codex,
                    info,
                    Record::CodexProjection(history.clone()),
                ));
            }
        }
    }

    fn harness(&mut self, root: &Path) {
        let registry = self.optional_index(read_json(&root.join("storages/workspace.json")));
        let mut memberships = HashMap::<&str, Vec<&str>>::new();
        if let Some(workspaces) = registry["tables"]["workspaces"].as_object() {
            for row in workspaces.values() {
                if let Some(path) = string(&row["path"]) {
                    for id in array(&row["sessionIds"]).iter().filter_map(string) {
                        memberships.entry(id).or_default().push(path);
                    }
                }
            }
        }

        // Only one format generation per session directory. Prefer the latest
        // generation and its compressed form when both are present.
        let mut files = HashMap::<PathBuf, ((u32, bool), PathBuf)>::new();
        for file in self.files(&root.join("sessions"), 2) {
            if let Some(version) = harness_version(&file) {
                let entry = files.entry(file.parent().unwrap().to_owned());
                entry
                    .and_modify(|candidate| {
                        if version > candidate.0 {
                            *candidate = (version, file.clone());
                        }
                    })
                    .or_insert((version, file));
            }
        }
        for (dir, (_, file)) in files {
            let fallback = dir.file_name().unwrap_or_default().to_string_lossy();
            let mut info = harness_header(&file);
            if info.skip {
                continue;
            }
            let id = info.id.take().unwrap_or_else(|| fallback.into_owned());
            let cache_path = file.file_name().map(|_| {
                root.join("storages/session_projcache/sessions")
                    .join(format!(
                        "{}.json",
                        dir.file_name().unwrap_or_default().to_string_lossy()
                    ))
            });
            let cache = cache_path
                .and_then(|path| read_json(&path).ok())
                .unwrap_or_default();
            let rows = &cache["record"]["rows"];
            if rows["sessionListMetadata"]["val"]["blank"].as_bool() == Some(true) {
                continue;
            }
            info.title = string(&rows["title"]["val"])
                .map(str::to_owned)
                .or(info.title);
            info.project = info
                .project
                .or_else(|| owned(&cache["record"]["identity"]["cwd"]));
            if let Some(paths) = memberships.get(id.as_str()) {
                if !paths
                    .iter()
                    .any(|path| Some(*path) == info.project.as_deref())
                    && paths.len() == 1
                {
                    info.project = Some(paths[0].to_owned());
                }
            }
            info.updated_at = modified(&file).max(
                rows["sessionListMetadata"]["val"]["lastPromptAt"]
                    .as_i64()
                    .unwrap_or_default(),
            );
            self.sessions.push(indexed(
                id,
                Source::Harness,
                Origin::Harness,
                info,
                Record::File(file),
            ));
        }
    }

    fn claude(&mut self, roots: &Roots) {
        let mut desktop = HashMap::<String, Vec<(PathBuf, Info)>>::new();
        for file in self.files(&roots.claude_desktop, 2) {
            if !extension(&file, "json") || !stem(&file).starts_with("local_") {
                continue;
            }
            let meta = match read_json(&file) {
                Ok(value) => value,
                Err(error) => {
                    self.warning.get_or_insert(error);
                    continue;
                }
            };
            if let Some(id) = string(&meta["cliSessionId"]) {
                let info = Info {
                    title: owned(&meta["title"]).or_else(|| owned(&meta["name"])),
                    project: owned(&meta["cwd"]).or_else(|| owned(&meta["workingDirectory"])),
                    updated_at: ms(&meta["updatedAt"]).max(modified(&file)),
                    ..Info::default()
                };
                desktop.entry(id.to_owned()).or_default().push((file, info));
            }
        }

        let projects = roots.claude_projects();
        let mut files = Vec::new();
        let mut titles = HashMap::new();
        for root in &projects {
            for file in self.files(root, 1) {
                if file.ends_with("sessions-index.json") {
                    let index = self.optional_index(read_json(&file));
                    for row in array(&index["entries"]) {
                        if let (Some(id), Some(title)) = (
                            string(&row["sessionId"]),
                            string(&row["customTitle"]).or_else(|| string(&row["summary"])),
                        ) {
                            titles.insert(
                                (file.parent().unwrap().to_owned(), id.to_owned()),
                                title.to_owned(),
                            );
                        }
                    }
                } else if extension(&file, "jsonl") {
                    files.push(file);
                }
            }
        }
        let mut known = HashSet::new();
        for file in files {
            if !known.insert(file.clone()) {
                continue;
            }
            let mut info = claude_header(&file);
            let id = info.id.take().unwrap_or_else(|| stem(&file));
            info.title = titles
                .remove(&(file.parent().unwrap().to_owned(), id.clone()))
                .or(info.title);
            let desktop_info = desktop.get(&id).or_else(|| desktop.get(&stem(&file)));
            if let Some((_, meta)) =
                desktop_info.and_then(|metas| metas.iter().max_by_key(|(_, info)| info.updated_at))
            {
                info.title = meta.title.clone().or(info.title);
                info.project = info.project.or_else(|| meta.project.clone());
                info.updated_at = meta.updated_at;
            }
            info.updated_at = info.updated_at.max(modified(&file));
            let origin = if info.desktop || desktop_info.is_some() {
                Origin::ClaudeDesktop
            } else {
                Origin::ClaudeCli
            };
            self.sessions.push(indexed(
                id,
                Source::Claude,
                origin,
                info,
                Record::File(file),
            ));
        }
        let present = self
            .sessions
            .iter()
            .map(|row| row.session.id.clone())
            .collect::<HashSet<_>>();
        for (id, metas) in desktop {
            if present.contains(&id) {
                continue;
            }
            for (file, mut info) in metas {
                let key = format!("claude-desktop:{}", file.display());
                info.id = Some(key);
                self.sessions.push(indexed(
                    id.clone(),
                    Source::Claude,
                    Origin::ClaudeDesktop,
                    info,
                    Record::ClaudeMissing(projects.clone()),
                ));
            }
        }
    }

    fn optional_index<T: Default>(&mut self, result: Result<T, Failure>) -> T {
        match result {
            Ok(index) => index,
            Err(error) => {
                if error != Failure::Missing {
                    self.warning.get_or_insert(error);
                }
                T::default()
            }
        }
    }
}

#[derive(Default)]
struct Info {
    id: Option<String>,
    title: Option<String>,
    project: Option<String>,
    updated_at: i64,
    desktop: bool,
    skip: bool,
}

fn indexed(id: String, source: Source, origin: Origin, info: Info, record: Record) -> Indexed {
    let key = match &record {
        Record::File(path) => path.to_string_lossy().into_owned(),
        Record::CodexProjection(_) => format!("codex-projection:{id}"),
        Record::CursorComposer { .. } => format!("cursor-composer:{id}"),
        Record::ClaudeMissing(_) => info.id.unwrap_or_else(|| id.clone()),
    };
    let project = info.project.filter(|path| !path.is_empty());
    let (worktree, worktree_repository) = project
        .as_deref()
        .and_then(vibe_worktree)
        .map(|(worktree, repo)| (Some(worktree), repo))
        .unwrap_or_default();
    let project_exists = project
        .as_deref()
        .is_some_and(|path| Path::new(path).is_dir());
    let repository = project
        .as_deref()
        .and_then(|path| git_root(Path::new(path), worktree.is_some()));
    Indexed {
        session: Session {
            key,
            title: info.title.unwrap_or_else(|| id.clone()),
            id,
            source,
            origin,
            project,
            project_exists,
            repository,
            worktree,
            worktree_repository,
            updated_at: info.updated_at,
        },
        record,
    }
}

fn vibe_worktree(path: &str) -> Option<(String, Option<String>)> {
    let parts: Vec<_> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let root = parts.iter().enumerate().position(|(i, part)| {
        *part == ".vibe-kanban-workspaces"
            || (*part == "worktrees" && i > 0 && parts[i - 1] == "vibe-kanban")
    })?;
    Some((
        parts.get(root + 1)?.to_string(),
        parts.get(root + 2).map(|name| name.to_string()),
    ))
}

fn git_root(path: &Path, worktree: bool) -> Option<String> {
    if !path.is_absolute() || !path.is_dir() {
        return None;
    }
    for dir in path.ancestors() {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Some(
                fs::canonicalize(dir)
                    .unwrap_or_else(|_| dir.to_owned())
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        if let Ok(pointer) = fs::read_to_string(&dot_git) {
            if worktree {
                let admin = dir.join(pointer.trim().strip_prefix("gitdir: ")?);
                let common = fs::read_to_string(admin.join("commondir")).ok()?;
                let git = fs::canonicalize(admin.join(common.trim())).ok()?;
                return git.parent().map(|root| root.to_string_lossy().into_owned());
            }
            return Some(dir.to_string_lossy().into_owned());
        }
    }
    None
}

pub fn string(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

fn owned(value: &Value) -> Option<String> {
    string(value).map(str::to_owned)
}

pub fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

pub fn ms(value: &Value) -> i64 {
    if let Some(number) = value.as_i64() {
        return if number.abs_diff(0) < 10_000_000_000 {
            number * 1000
        } else {
            number
        };
    }
    value
        .as_str()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map_or(0, |time| time.timestamp_millis())
}

fn modified(file: &Path) -> i64 {
    fs::metadata(file)
        .and_then(|value| value.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_millis() as i64)
}

fn read_json(path: &Path) -> Result<Value, Failure> {
    serde_json::from_reader(BufReader::new(File::open(path)?)).map_err(|_| Failure::InvalidRecord)
}

pub fn json_lines(path: &Path) -> Result<impl Iterator<Item = Result<Value, Failure>>, Failure> {
    let file = BufReader::new(File::open(path)?);
    let reader: Box<dyn BufRead> = if extension(path, "zstd") || extension(path, "zst") {
        Box::new(BufReader::new(zstd::Decoder::with_buffer(file)?))
    } else {
        Box::new(file)
    };
    Ok(reader.lines().filter_map(|line| match line {
        Ok(line) if line.trim().is_empty() => None,
        Ok(line) => Some(
            serde_json::from_str::<Value>(&line)
                .ok()
                .filter(Value::is_object)
                .ok_or(Failure::InvalidRecord),
        ),
        Err(error) => Some(Err(error.into())),
    }))
}

fn extension(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|value| value == ext)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn rollout_id(file: &Path) -> String {
    let name = stem(file);
    if name.starts_with("rollout-") && name.len() > 36 && name.is_char_boundary(name.len() - 36) {
        return name[name.len() - 36..].to_owned();
    }
    name
}

fn codex_header(file: &Path) -> Info {
    let mut info = Info::default();
    if let Ok(lines) = json_lines(file) {
        for row in lines.take(32).map_while(Result::ok) {
            let p = &row["payload"];
            if row["type"] == "session_meta" {
                info.id = owned(&p["id"]).or_else(|| owned(&p["session_id"]));
                info.project = owned(&p["cwd"]);
                info.skip = p["source"].get("subagent").is_some();
                if info.skip {
                    break;
                }
            } else if p["type"] == "user_message" {
                info.title = owned(&p["message"]);
            } else if p["role"] == "user" {
                info.title = first_text(&p["content"])
                    .filter(|s| !s.starts_with("<environment_context>"))
                    .map(str::to_owned);
            }
            if info.title.is_some() {
                break;
            }
        }
    }
    info
}

fn harness_version(file: &Path) -> Option<(u32, bool)> {
    let name = file.file_name()?.to_str()?;
    let (name, compressed) = name
        .strip_suffix(".zstd")
        .or_else(|| name.strip_suffix(".zst"))
        .map(|name| (name, true))
        .unwrap_or((name, false));
    if name == "session.jsonl" {
        return Some((0, compressed));
    }
    let number = name.strip_prefix("session.v")?.strip_suffix(".jsonl")?;
    Some((number.parse().ok()?, compressed))
}

fn harness_header(file: &Path) -> Info {
    let mut info = Info::default();
    if let Ok(lines) = json_lines(file) {
        for row in lines.take(32).map_while(Result::ok) {
            if row["type"] == "session" {
                info.id = owned(&row["id"]);
                info.project = owned(&row["cwd"]);
                info.skip = row["origin"] == "subagent";
                if info.skip {
                    break;
                }
            } else if row["type"] == "session/title" {
                info.title = owned(&row["data"]["title"]).or(info.title);
            } else if info.title.is_none()
                && row["type"] == "user/message"
                && row["data"]["source"]["kind"] == "user"
            {
                info.title = first_text(&row["data"]["content"]).map(str::to_owned);
            }
        }
    }
    info
}

fn claude_header(file: &Path) -> Info {
    let mut info = Info::default();
    if let Ok(lines) = json_lines(file) {
        for row in lines.take(64).map_while(Result::ok) {
            info.id = info.id.or_else(|| owned(&row["sessionId"]));
            info.project = info.project.or_else(|| owned(&row["cwd"]));
            info.desktop |= row["entrypoint"] == "claude-desktop";
            if row["type"] == "custom-title" {
                info.title = owned(&row["customTitle"]).or(info.title);
            } else if info.title.is_none() && row["type"] == "user" && row["isMeta"] != true {
                info.title = first_text(&row["message"]["content"])
                    .filter(|text| !text.starts_with("<local-command"))
                    .map(|text| text.lines().next().unwrap_or(text).to_owned());
            }
        }
    }
    info
}

pub fn find_claude_transcript(projects: &[PathBuf], id: &str) -> Result<PathBuf, Failure> {
    if Path::new(id).file_name().and_then(|name| name.to_str()) != Some(id) {
        return Err(Failure::InvalidRecord);
    }
    let filename = format!("{id}.jsonl");
    for project in projects {
        let dirs = match fs::read_dir(project) {
            Ok(dirs) => dirs,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for dir in dirs {
            let dir = dir?;
            if !dir.file_type()?.is_dir() {
                continue;
            }
            let candidate = dir.path().join(&filename);
            match fs::symlink_metadata(&candidate) {
                Ok(meta) if meta.is_file() => return Ok(candidate),
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Err(Failure::Missing)
}

fn first_text(value: &Value) -> Option<&str> {
    string(value).or_else(|| {
        array(value).iter().find_map(|part| {
            matches!(
                part["type"].as_str(),
                Some("text" | "input_text" | "output_text" | "Text")
            )
            .then(|| string(&part["text"]))
            .flatten()
        })
    })
}

fn newest_db(root: &Path, prefix: &str, warning: &mut Option<Failure>) -> Option<PathBuf> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            warning.get_or_insert(error.into());
            return None;
        }
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let version: u32 = path
                .file_name()?
                .to_str()?
                .strip_prefix(prefix)?
                .strip_suffix(".sqlite")?
                .parse()
                .ok()?;
            Some((version, path))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
}

pub fn readonly_db(path: &Path) -> Result<Connection, Failure> {
    let path = fs::canonicalize(path)?;
    let wal = PathBuf::from(format!("{}-wal", path.to_string_lossy()));
    let has_wal = match fs::metadata(wal) {
        Ok(meta) => meta.len() > 0,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let mut uri = tauri::Url::from_file_path(&path).map_err(|_| Failure::Index)?;
    // The live WAL is read through a read-only shared-memory mapping. Without
    // a WAL, immutable mode also avoids creating shared-memory sidecars.
    uri.set_query(Some(if has_wal {
        "mode=ro&readonly_shm=1"
    } else {
        "mode=ro&immutable=1"
    }));
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| Failure::Index)
}

fn codex_index(path: &Path) -> Result<HashMap<String, Info>, Failure> {
    let db = readonly_db(path)?;
    let mut schema = db
        .prepare("PRAGMA table_info(threads)")
        .map_err(|_| Failure::Index)?;
    let columns = schema
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| Failure::Index)?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|_| Failure::Index)?;
    let mut titles = Vec::new();
    for name in ["name", "title", "preview", "first_user_message"] {
        if columns.contains(name) {
            titles.push(format!("NULLIF({name}, '')"));
        }
    }
    titles.push("NULL".into());
    let title = if titles.len() > 1 {
        format!("COALESCE({})", titles.join(", "))
    } else {
        "NULL".into()
    };
    let mut query = db
        .prepare(&format!(
            "SELECT id, cwd, {title}, updated_at, source FROM threads"
        ))
        .map_err(|_| Failure::Index)?;
    let rows = query
        .query_map([], |row| {
            let source: String = row.get(4)?;
            let updated_at: i64 = row.get(3)?;
            Ok((
                row.get(0)?,
                Info {
                    project: row.get(1)?,
                    title: row.get(2)?,
                    updated_at: updated_at.saturating_mul(1000),
                    skip: source.contains("\"subagent\""),
                    ..Info::default()
                },
            ))
        })
        .map_err(|_| Failure::Index)?;
    rows.collect::<Result<_, _>>().map_err(|_| Failure::Index)
}

fn projected_ids(path: &Path) -> Result<HashSet<String>, Failure> {
    let db = readonly_db(path)?;
    let mut query = db
        .prepare("SELECT DISTINCT thread_id FROM thread_items")
        .map_err(|_| Failure::Index)?;
    let rows = query
        .query_map([], |row| row.get(0))
        .map_err(|_| Failure::Index)?;
    rows.collect::<Result<_, _>>().map_err(|_| Failure::Index)
}

pub fn projected_events(path: &Path, id: &str) -> Result<Vec<Value>, Failure> {
    let db = readonly_db(path)?;
    let mut statement = db.prepare(
        "SELECT item_json, created_at_ms FROM thread_items WHERE thread_id = ?1 ORDER BY rollout_ordinal, created_at_ms, item_id"
    ).map_err(|_| Failure::Index)?;
    let rows = statement
        .query_map([id], |row| {
            let raw: String = row.get(0)?;
            let time: i64 = row.get(1)?;
            Ok((raw, time))
        })
        .map_err(|_| Failure::Index)?;
    let mut events = Vec::new();
    for row in rows {
        let (raw, time) = row.map_err(|_| Failure::Index)?;
        let mut event: Value = serde_json::from_str(&raw).map_err(|_| Failure::InvalidRecord)?;
        // Keep the native projected item object exactly as stored. Supply
        // time from its SQLite row separately to the reader.
        if !event.is_object() {
            return Err(Failure::InvalidRecord);
        }
        event = serde_json::json!({ "item": event, "time": time });
        events.push(event);
    }
    if events.is_empty() {
        return Err(Failure::Missing);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn fixture_roots(home: &Path) -> Roots {
        Roots {
            codex: home.join("codex"),
            harness: home.join("harness"),
            claude: home.join("claude"),
            claude_default: home.join("claude"),
            claude_desktop: home.join("desktop"),
            workbuddy: home.join("workbuddy"),
            cursor: home.join("cursor"),
            cursor_user: home.join("cursor-user"),
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn snapshot(item: &Indexed) -> Value {
        serde_json::to_value(crate::transcript::read(item.clone()).unwrap()).unwrap()
    }

    #[test]
    fn indexes_beyond_a_page_and_reads_codex_projection_with_a_live_wal() {
        let home = tempfile::tempdir().unwrap();
        let roots = fixture_roots(home.path());
        let home = home.path().to_string_lossy();
        let missing_project = format!("{home}/deleted");
        for n in 0..105 {
            let header = json!({"type":"session_meta","payload":{"id":format!("thread-{n}"),"cwd":missing_project}});
            write(
                &roots.codex.join(format!("sessions/2026/11/19/{n}.jsonl")),
                format!("{header}\n").as_bytes(),
            );
        }
        let state = Connection::open(roots.codex.join("state_5.sqlite")).unwrap();
        state
            .execute_batch(
                "PRAGMA journal_mode = WAL;
            CREATE TABLE threads(id TEXT, cwd TEXT, updated_at INTEGER, title TEXT, source TEXT);
            INSERT INTO threads VALUES ('projected', '', 1, 'Projected name', 'vscode');",
            )
            .unwrap();
        let path = roots.codex.join("thread_history_1.sqlite");
        let history = Connection::open(&path).unwrap();
        history.execute_batch("PRAGMA journal_mode = WAL;
            CREATE TABLE thread_items(thread_id TEXT, item_id TEXT, rollout_ordinal INTEGER, created_at_ms INTEGER, item_json TEXT);
        ").unwrap();
        history
            .execute(
                "INSERT INTO thread_items VALUES ('projected', '2', 2, 1795060800001, ?1)",
                [json!({"type":"agentMessage","text":"last"}).to_string()],
            )
            .unwrap();
        history
            .execute(
                "INSERT INTO thread_items VALUES ('projected', '1', 1, 1794974400000, ?1)",
                [
                    json!({"type":"userMessage","content":[{"type":"text","text":"first"}]})
                        .to_string(),
                ],
            )
            .unwrap();
        let sidecars = [
            path.clone(),
            path.with_extension("sqlite-shm"),
            path.with_extension("sqlite-wal"),
        ];
        let before = sidecars.each_ref().map(|file| {
            (
                fs::read(file).unwrap(),
                fs::metadata(file).unwrap().modified().unwrap(),
            )
        });
        // SQLite shares the writer's writable mapping inside its own
        // process. Run the reader in another process, as the app does.
        let reader = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "catalog::tests::check_live_codex_fixture",
            ])
            .env("SESSION_TAKE_CODEX_FIXTURE", home.as_ref())
            .output()
            .unwrap();
        assert!(
            reader.status.success(),
            "{}",
            String::from_utf8_lossy(&reader.stdout)
        );
        for (file, (bytes, time)) in sidecars.iter().zip(before) {
            assert!(
                fs::read(file).unwrap() == bytes,
                "reader wrote to {:?}",
                file.extension()
            );
            assert_eq!(fs::metadata(file).unwrap().modified().unwrap(), time);
        }
    }

    #[test]
    #[ignore = "invoked by the live WAL fixture"]
    fn check_live_codex_fixture() {
        let Ok(home) = std::env::var("SESSION_TAKE_CODEX_FIXTURE") else {
            return;
        };
        let roots = fixture_roots(Path::new(&home));
        let catalog = Catalog::load(&roots, Source::Codex);
        assert_eq!(catalog.warning, None);
        assert_eq!(catalog.sessions.len(), 106);
        let item = catalog
            .sessions
            .iter()
            .find(|item| item.session.id == "projected")
            .unwrap();
        assert_eq!(item.session.title, "Projected name");
        assert!(item.session.project.is_none());
        let view = snapshot(item);
        assert_eq!(view["entries"][0]["text"], "first");
        assert_eq!(view["entries"][1]["text"], "last");
        let native: Value =
            serde_json::from_str(view["jsonl"].as_str().unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(native["type"], "userMessage");
        let file = catalog
            .sessions
            .iter()
            .find(|item| item.session.id == "thread-104")
            .unwrap();
        assert_eq!(
            file.session.project.as_deref(),
            Some(format!("{home}/deleted").as_str())
        );
        assert!(!file.session.project_exists);
    }

    #[test]
    fn harness_reads_compressed_events_with_workspace_membership() {
        let home = tempfile::tempdir().unwrap();
        let roots = fixture_roots(home.path());
        let path = roots
            .harness
            .join("sessions/old/session-1/session.v4.jsonl.zstd");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut encoder = zstd::Encoder::new(File::create(path).unwrap(), 0).unwrap();
        writeln!(
            encoder,
            "{}",
            json!({"type":"session","id":"session-1","cwd":"/old/checkout"})
        )
        .unwrap();
        writeln!(encoder, "{}", json!({"type":"user/message","time":1794974400000i64,"data":{"role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"question"}]}})).unwrap();
        writeln!(encoder, "{}", json!({"type":"developer/message","data":{"message":{"role":"developer","content":"hidden"}}})).unwrap();
        writeln!(encoder, "{}", json!({"type":"tool/call","data":{"name":"Bash","callId":"a","arguments":"{\"command\":\"echo first\"}"}})).unwrap();
        writeln!(encoder, "{}", json!({"type":"tool/call","data":{"name":"Bash","callId":"b","arguments":"{\"command\":\"echo second\"}"}})).unwrap();
        writeln!(encoder, "{}", json!({"type":"tool/result","data":{"message":{"role":"tool","toolCallId":"b","content":[{"type":"text","text":"second\nline"}]}}})).unwrap();
        writeln!(encoder, "{}", json!({"type":"tool/result","data":{"message":{"role":"tool","toolCallId":"a","content":[{"type":"text","text":"first"}]}}})).unwrap();
        encoder.finish().unwrap();
        write(&roots.harness.join("storages/workspace.json"),
            br#"{"tables":{"workspaces":{"workspace":{"path":"/deleted/repo","sessionIds":["session-1"]}}}}"#);
        let catalog = Catalog::load(&roots, Source::Harness);
        assert_eq!(catalog.sessions.len(), 1);
        assert_eq!(
            catalog.sessions[0].session.project.as_deref(),
            Some("/deleted/repo")
        );
        let view = snapshot(&catalog.sessions[0]);
        assert!(!view["jsonl"].as_str().unwrap().contains("hidden"));
        assert_eq!(view["entries"][1]["input"], "echo first");
        assert!(view["entries"][1]["time"].is_null());
        assert_eq!(view["entries"][3]["callId"], "b");
        assert_eq!(view["entries"][3]["output"], "second\nline");
        assert_eq!(view["entries"][4]["callId"], "a");
    }

    #[test]
    fn workbuddy_reads_messages_and_matches_results_after_interleaved_calls() {
        let home = tempfile::tempdir().unwrap();
        let roots = fixture_roots(home.path());
        let path = roots.workbuddy.join("projects/old-repo/wb.jsonl");
        write(&path, br#"{"type":"message","role":"system","content":"hidden"}
{"type":"message","role":"user","cwd":"/deleted/repo","sessionId":"wb","timestamp":1794974400000,"content":[{"type":"input_text","text":"<system-reminder>hidden</system-reminder><user_query>check files</user_query>"}]}
{"type":"function_call","id":"event-a","callId":"call-a","name":"Shell","arguments":"{\"command\":\"pwd\"}"}
{"type":"function_call","id":"event-b","callId":"call-b","name":"Read","arguments":{"path":"a"}}
{"type":"function_call_result","parentId":"event-b","output":{"type":"text","text":"file contents"}}
{"type":"function_call_result","callId":"call-a","output":{"type":"text","text":"/repo"}}
{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}
{"type":"ai-title","aiTitle":"File check"}"#);
        let catalog = Catalog::load(&roots, Source::Workbuddy);
        assert_eq!(catalog.warning, None);
        assert_eq!(catalog.sessions.len(), 1);
        let item = &catalog.sessions[0];
        assert_eq!(item.session.project.as_deref(), Some("/deleted/repo"));
        assert_eq!(item.session.title, "File check");
        let read = crate::transcript::read(item.clone()).unwrap();
        assert!(read.markdown().contains("Tool: WorkBuddy"));
        assert!(!read.jsonl.contains("hidden"));
        assert!(read.jsonl.contains("<user_query>check files</user_query>"));
        let entries = serde_json::to_value(read.entries).unwrap();
        assert_eq!(entries[0]["text"], "check files");
        assert!(entries[0]["time"].is_string());
        assert_eq!(entries[1]["input"], "pwd");
        assert_eq!(entries[3]["callId"], "call-b");
        assert_eq!(entries[3]["output"], "file contents");
        assert_eq!(entries[4]["callId"], "call-a");
        assert_eq!(entries[5]["text"], "done");
        let db = Connection::open(roots.workbuddy.join("workbuddy.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE sessions(id,cwd,title,custom_title);
            INSERT INTO sessions VALUES('wb','/deleted/repo','auto','My title')",
        )
        .unwrap();
        let catalog = Catalog::load(&roots, Source::Workbuddy);
        assert_eq!(catalog.sessions[0].session.title, "My title");
    }

    #[test]
    fn cursor_indexes_both_stores_and_falls_back_when_a_bubble_is_missing() {
        let home = tempfile::tempdir().unwrap();
        let roots = fixture_roots(home.path());
        let dir = roots.cursor.join("projects/deleted-repo/agent-transcripts");
        write(&dir.join("chat/chat.jsonl"), br#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>mirror prompt</user_query>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"mirror reply"},{"type":"tool_use","name":"Read","input":{"path":"a"}}]}}"#);
        write(&dir.join("chat.jsonl"), b"older flat copy");
        write(&dir.join("chat/subagents/side.jsonl"), b"subagent");
        write(
            &dir.join("flat.jsonl"),
            br#"{"role":"user","message":{"content":"other chat"}}"#,
        );
        write(
            &roots
                .cursor_user
                .join("workspaceStorage/old/workspace.json"),
            br#"{"folder":"file:///deleted/repo"}"#,
        );
        let dbfile = roots.cursor_user.join("globalStorage/state.vscdb");
        fs::create_dir_all(dbfile.parent().unwrap()).unwrap();
        let db = Connection::open(dbfile).unwrap();
        db.execute_batch(r#"CREATE TABLE cursorDiskKV(key TEXT PRIMARY KEY, value BLOB);
            INSERT INTO cursorDiskKV VALUES
            ('composerData:chat','{"name":"Named chat","fullConversationHeadersOnly":[{"bubbleId":"z"},{"bubbleId":"a"}]}'),
            ('bubbleId:chat:a','{"type":2,"bubbleId":"a","text":"answer","toolFormerData":{"toolCallId":"t","name":"Read","params":{"path":"a"},"result":"contents"}}'),
            ('bubbleId:chat:z','{"type":1,"bubbleId":"z","text":"question"}'),
            ('composerData:legacy','{"conversation":[{"type":1,"text":"old question"}]}');"#).unwrap();
        let catalog = Catalog::load(&roots, Source::Cursor);
        assert_eq!(catalog.warning, None);
        assert_eq!(catalog.sessions.len(), 3);
        let get = |id| {
            catalog
                .sessions
                .iter()
                .find(|s| s.session.id == id)
                .unwrap()
        };
        assert_eq!(
            snapshot(get("legacy"))["entries"][0]["text"],
            "old question"
        );
        assert_eq!(
            get("flat").session.project.as_deref(),
            Some("/deleted/repo")
        );
        let item = get("chat");
        assert_eq!(item.session.title, "Named chat");
        let view = snapshot(item);
        assert_eq!(view["entries"][0]["text"], "question");
        assert_eq!(view["entries"][1]["text"], "answer");
        assert_eq!(view["entries"][3]["output"], "contents");
        assert_eq!(view["jsonl"].as_str().unwrap().lines().count(), 2);
        db.execute("DELETE FROM cursorDiskKV WHERE key='bubbleId:chat:a'", [])
            .unwrap();
        let read = crate::transcript::read(item.clone()).unwrap();
        assert!(read.markdown().contains("Tool: Cursor"));
        let rows = serde_json::to_value(read.entries).unwrap();
        assert_eq!(rows[0]["text"], "mirror prompt");
        assert_eq!(rows[1]["text"], "mirror reply");
        assert_eq!(rows[2]["name"], "Read");
    }

    #[test]
    fn claude_distinguishes_cli_and_desktop_and_can_retry_a_damaged_file() {
        let home = tempfile::tempdir().unwrap();
        let roots = fixture_roots(home.path());
        let desktop = roots.claude.join("projects/deleted/desktop-id.jsonl");
        write(&desktop, b"{ damaged");
        write(
            &roots
                .claude_desktop
                .join("account/workspace/local_test.json"),
            br#"{"cliSessionId":"desktop-id","title":"Desktop title","cwd":"/deleted/repo"}"#,
        );
        write(&roots.claude.join("projects/deleted/cli-id.jsonl"),
            br#"{"type":"user","message":{"role":"user","content":"CLI prompt"},"sessionId":"cli-id","cwd":"/deleted/repo"}"#);
        let catalog = Catalog::load(&roots, Source::Claude);
        let desk = catalog
            .sessions
            .iter()
            .find(|item| item.session.id == "desktop-id")
            .unwrap();
        let cli = catalog
            .sessions
            .iter()
            .find(|item| item.session.id == "cli-id")
            .unwrap();
        assert!(matches!(desk.session.origin, Origin::ClaudeDesktop));
        assert!(matches!(cli.session.origin, Origin::ClaudeCli));
        assert_eq!(
            crate::transcript::read(desk.clone()).err(),
            Some(Failure::InvalidRecord)
        );
        assert_eq!(snapshot(cli)["entries"][0]["text"], "CLI prompt");
        write(&desktop,
            br#"{"type":"user","sessionId":"desktop-id","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"a","content":"full result"}]}}"#);
        assert_eq!(snapshot(desk)["entries"][0]["output"], "full result");
    }

    #[test]
    #[ignore = "reads the locally installed tools' histories"]
    fn verify_local_histories_without_printing_contents() {
        let roots = Roots::local().unwrap();
        for source in [Source::Codex, Source::Harness, Source::Claude] {
            let catalog = Catalog::load(&roots, source);
            assert_eq!(catalog.warning, None, "{source:?}");
            let mut coverage = (0, 0, 0, 0);
            for item in &catalog.sessions {
                if matches!(item.record, Record::CodexProjection(_)) {
                    coverage.0 += 1;
                }
                let Ok(snapshot) = crate::transcript::read(item.clone()) else {
                    coverage.3 += 1;
                    continue;
                };
                let view = serde_json::to_value(snapshot).unwrap();
                if view["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["kind"] == "message")
                {
                    coverage.1 += 1;
                }
                if view["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["kind"] == "result")
                {
                    coverage.2 += 1;
                }
                for line in view["jsonl"].as_str().unwrap().lines() {
                    assert!(serde_json::from_str::<Value>(line).unwrap().is_object());
                }
            }
            println!("{source:?}: {} indexed, {} projected, {} with messages, {} with tool results, {} unreadable",
                catalog.sessions.len(), coverage.0, coverage.1, coverage.2, coverage.3);
            assert!(coverage.1 > 0 && coverage.2 > 0);
        }
    }
}
