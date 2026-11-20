use super::*;
use rusqlite::OptionalExtension;

// Only unwrap complete, leading prompt envelopes. Literal tags in the user's
// prose and code samples are part of the conversation.
pub(crate) fn without_reminders(text: &str) -> &str {
    let mut rest = text.trim();
    while let Some(body) = rest.strip_prefix("<system-reminder>") {
        let Some((_, tail)) = body.split_once("</system-reminder>") else {
            break;
        };
        rest = tail.trim();
    }
    if rest == text.trim() {
        text
    } else {
        rest
    }
}

pub(crate) fn user_prompt(text: &str) -> &str {
    let rest = without_reminders(text);
    rest.trim()
        .strip_prefix("<user_query>")
        .and_then(|query| query.strip_suffix("</user_query>"))
        .map(str::trim)
        .unwrap_or(rest)
}

fn file_info(path: &Path, scan_titles: bool) -> Info {
    let mut info = Info {
        updated_at: modified(path),
        ..Info::default()
    };
    if let Ok(rows) = json_lines(path) {
        for row in rows.map_while(Result::ok) {
            info.id = info.id.or_else(|| owned(&row["sessionId"]));
            info.project = info.project.or_else(|| owned(&row["cwd"]));
            if row["type"] == "ai-title" {
                info.title = owned(&row["aiTitle"]).or(info.title);
            } else if row["role"] == "user" && info.title.is_none() {
                let body = row.get("message").unwrap_or(&row);
                info.title = first_text(&body["content"])
                    .map(user_prompt)
                    .and_then(|s| s.trim().lines().next())
                    .map(|line| line.chars().take(120).collect());
            }
            if !scan_titles && info.title.is_some() {
                break;
            }
        }
    }
    info
}

fn workbuddy_info(path: &Path) -> Result<HashMap<String, Info>, Failure> {
    let db = readonly_db(path)?;
    let mut stmt = db
        .prepare("SELECT id, cwd, COALESCE(NULLIF(custom_title,''), title) FROM sessions")
        .map_err(|_| Failure::Index)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Info {
                    project: row.get(1)?,
                    title: row.get(2)?,
                    ..Info::default()
                },
            ))
        })
        .map_err(|_| Failure::Index)?;
    rows.collect::<Result<_, _>>().map_err(|_| Failure::Index)
}

impl Catalog {
    pub(super) fn workbuddy(&mut self, root: &Path) {
        let metadata = self.optional_index(workbuddy_info(&root.join("workbuddy.db")));
        for path in self.files(&root.join("projects"), 1) {
            if !extension(&path, "jsonl") {
                continue;
            }
            let mut id = stem(&path);
            let mut info = file_info(&path, !metadata.contains_key(&id));
            if let Some(native_id) = info.id.take() {
                id = native_id;
            }
            if let Some(stored) = metadata.get(&id) {
                info.title = stored
                    .title
                    .clone()
                    .filter(|s| !s.is_empty())
                    .or(info.title);
                info.project = stored
                    .project
                    .clone()
                    .filter(|s| !s.is_empty())
                    .or(info.project);
            }
            self.sessions.push(indexed(
                id,
                Source::Workbuddy,
                Origin::Workbuddy,
                info,
                Record::File(path),
            ));
        }
    }

    pub(super) fn cursor(&mut self, roots: &Roots) {
        let db = roots.cursor_user.join("globalStorage/state.vscdb");
        let mut composers = self.optional_index(cursor_index(&db));
        let workspaces = self.cursor_workspaces(&roots.cursor_user, &mut composers);
        let mut transcripts = HashMap::new();
        let projects = roots.cursor.join("projects");
        let dirs = match fs::read_dir(&projects) {
            Ok(dirs) => Some(dirs),
            Err(err) => {
                self.optional_index::<()>(Err(err.into()));
                None
            }
        };
        for entry in dirs.into_iter().flatten() {
            let project = match entry.and_then(|e| Ok((e.file_type()?, e.path()))) {
                Ok((ty, path)) if ty.is_dir() => path,
                Ok(_) => continue,
                Err(err) => {
                    self.warning.get_or_insert(err.into());
                    continue;
                }
            };
            let dir = project.join("agent-transcripts");
            for path in self.files(&dir, 1) {
                if !extension(&path, "jsonl") {
                    continue;
                }
                let id = stem(&path);
                // Current versions nest the main transcript under its own ID.
                // Ignore unrelated JSONL and prefer the nested copy on migration.
                if path.parent() == Some(dir.as_path()) {
                    transcripts.entry(id).or_insert(path);
                } else if path.parent() == Some(dir.join(&id).as_path()) {
                    transcripts.insert(id, path);
                }
            }
        }
        for (id, path) in transcripts {
            let mut info = file_info(&path, false);
            if info.project.is_none() {
                let slug = path
                    .strip_prefix(&projects)
                    .ok()
                    .and_then(|p| p.iter().next())
                    .and_then(|s| s.to_str());
                info.project = slug.and_then(|s| workspaces.get(s)).cloned().flatten();
            }
            let record = if let Some(stored) = composers.remove(&id) {
                info.title = stored.title.or(info.title);
                info.project = stored.project.or(info.project);
                info.updated_at = info.updated_at.max(stored.updated_at);
                Record::CursorComposer {
                    db: db.clone(),
                    transcript: Some(path),
                }
            } else {
                Record::File(path)
            };
            self.sessions
                .push(indexed(id, Source::Cursor, Origin::Cursor, info, record));
        }
        for (id, info) in composers {
            self.sessions.push(indexed(
                id,
                Source::Cursor,
                Origin::Cursor,
                info,
                Record::CursorComposer {
                    db: db.clone(),
                    transcript: None,
                },
            ));
        }
    }

    fn cursor_workspaces(
        &mut self,
        user: &Path,
        composers: &mut HashMap<String, Info>,
    ) -> HashMap<String, Option<String>> {
        let mut paths = HashMap::new();
        for file in self.files(&user.join("workspaceStorage"), 1) {
            if !file.ends_with("workspace.json") {
                continue;
            }
            let workspace = self.optional_index(read_json(&file));
            let Some(path) = string(&workspace["folder"])
                .and_then(|s| tauri::Url::parse(s).ok())
                .and_then(|u| u.to_file_path().ok())
            else {
                continue;
            };
            let path = path.to_string_lossy().into_owned();
            // Slugs lose punctuation. Resolve only an unambiguous known path.
            let slug = path.trim_start_matches('/').replace(['/', '.', '_'], "-");
            paths
                .entry(slug)
                .and_modify(|known: &mut Option<String>| {
                    if known.as_ref() != Some(&path) {
                        *known = None;
                    }
                })
                .or_insert_with(|| Some(path.clone()));
            if composers.is_empty() {
                continue;
            }
            let db = file.with_file_name("state.vscdb");
            let headers = self.optional_index(
                readonly_db(&db).and_then(|db| cursor_headers(&db, "composer.composerData")),
            );
            for row in array(&headers["allComposers"]) {
                if let Some(info) = string(&row["composerId"]).and_then(|id| composers.get_mut(id))
                {
                    info.project.get_or_insert_with(|| path.clone());
                    info.title = info.title.take().or_else(|| owned(&row["name"]));
                }
            }
        }
        paths
    }
}

fn cursor_index(path: &Path) -> Result<HashMap<String, Info>, Failure> {
    let db = readonly_db(path)?;
    let mut info = HashMap::new();
    // Keep bubble text and inline histories out of the list response.
    let mut query = db.prepare("SELECT substr(key,14), json_extract(CAST(value AS TEXT),'$.name'),
        CAST(COALESCE(json_extract(CAST(value AS TEXT),'$.lastUpdatedAt'), json_extract(CAST(value AS TEXT),'$.createdAt')) AS TEXT),
        json_extract(CAST(value AS TEXT),'$.workspaceIdentifier.uri.fsPath')
        FROM cursorDiskKV WHERE key GLOB 'composerData:*' AND json_valid(CAST(value AS TEXT))
        AND (json_array_length(CAST(value AS TEXT),'$.fullConversationHeadersOnly') > 0
          OR json_array_length(CAST(value AS TEXT),'$.conversation') > 0)").map_err(|_| Failure::Index)?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Info {
                    title: row.get(1)?,
                    updated_at: row.get::<_, Option<String>>(2)?.map_or(0, |v| {
                        v.parse::<i64>().unwrap_or_else(|_| ms(&Value::String(v)))
                    }),
                    project: row.get(3)?,
                    ..Info::default()
                },
            ))
        })
        .map_err(|_| Failure::Index)?;
    for row in rows {
        let (id, entry) = row.map_err(|_| Failure::Index)?;
        info.insert(id, entry);
    }
    let data = cursor_headers(&db, "composer.composerHeaders").unwrap_or_default();
    for row in array(&data["allComposers"]) {
        if let Some(entry) = string(&row["composerId"]).and_then(|id| info.get_mut(id)) {
            entry.project =
                owned(&row["workspaceIdentifier"]["uri"]["fsPath"]).or(entry.project.take());
            entry.title = owned(&row["name"]).or(entry.title.take());
            entry.updated_at = entry.updated_at.max(ms(&row["lastUpdatedAt"]));
        }
    }
    Ok(info)
}

fn cursor_headers(db: &Connection, key: &str) -> Result<Value, Failure> {
    if !db
        .table_exists(None, "ItemTable")
        .map_err(|_| Failure::Index)?
    {
        return Ok(Value::Null);
    }
    let raw: Option<String> = db
        .query_row(
            "SELECT CAST(value AS TEXT) FROM ItemTable WHERE key=?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Failure::Index)?;
    raw.map(|s| serde_json::from_str(&s).map_err(|_| Failure::InvalidRecord))
        .transpose()
        .map(Option::unwrap_or_default)
}

pub fn cursor_events(path: &Path, id: &str) -> Result<Vec<Value>, Failure> {
    let mut db = readonly_db(path)?;
    let tx = db.transaction().map_err(|_| Failure::Index)?;
    let mut stmt = tx
        .prepare("SELECT CAST(value AS TEXT) FROM cursorDiskKV WHERE key=?1")
        .map_err(|_| Failure::Index)?;
    let mut get = |key: String| -> Result<Value, Failure> {
        let data: String = stmt
            .query_row([key], |row| row.get(0))
            .optional()
            .map_err(|_| Failure::Index)?
            .ok_or(Failure::Missing)?;
        serde_json::from_str(&data).map_err(|_| Failure::InvalidRecord)
    };
    let composer = get(format!("composerData:{id}"))?;
    let headers = array(&composer["fullConversationHeadersOnly"]);
    if headers.is_empty() {
        return composer["conversation"]
            .as_array()
            .filter(|a| !a.is_empty())
            .cloned()
            .ok_or(Failure::InvalidRecord);
    }
    let mut events = Vec::new();
    for header in headers {
        if header["isRenderable"] == false {
            continue;
        }
        let bubble = string(&header["bubbleId"]).ok_or(Failure::InvalidRecord)?;
        let row = match composer["conversationMap"].get(bubble) {
            Some(value) if value.is_object() && value.get("type").is_some() => value.clone(),
            _ => get(format!("bubbleId:{id}:{bubble}"))?,
        };
        events.push(row);
    }
    Ok(events)
}
