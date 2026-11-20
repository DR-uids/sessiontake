use crate::context::{digest, Context};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    #[default]
    Codex,
    Harness,
}

pub struct Record {
    pub source: Source,
    pub project_id: Option<String>,
    pub native_id: Option<String>,
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub target_project: Option<String>,
    pub body: Option<String>,
    pub stage: String,
}
pub struct Records(Connection);
type Result<T> = std::result::Result<T, String>;
impl Records {
    pub fn open(path: &Path) -> Result<Self> {
        let mut c = Connection::open(path).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        c.execute_batch(
            "PRAGMA secure_delete=ON;
            CREATE TABLE IF NOT EXISTS handoffs(
                id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL UNIQUE, source_id TEXT NOT NULL,
                version TEXT NOT NULL, title TEXT NOT NULL, cwd TEXT NOT NULL, body TEXT,
                target_project TEXT, body_hash TEXT NOT NULL, stage TEXT NOT NULL, error TEXT);",
        )
        .map_err(|e| e.to_string())?;
        let migration = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        for (column, definition) in [
            ("target_project", "TEXT"),
            ("source_tool", "TEXT NOT NULL DEFAULT 'codex'"),
            ("project_id", "TEXT"),
            ("native_id", "TEXT"),
        ] {
            let exists: bool = migration
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('handoffs') WHERE name=?1)",
                    [column],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if !exists {
                migration
                    .execute(
                        &format!("ALTER TABLE handoffs ADD COLUMN {column} {definition}"),
                        [],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        migration.commit().map_err(|e| e.to_string())?;
        Ok(Self(c))
    }
    pub fn prepare(
        &self,
        source: &str,
        additional: &str,
        target_project: Option<&str>,
        ctx: Context,
    ) -> Result<Record> {
        self.prepare_for(Source::Codex, source, additional, target_project, None, ctx)
    }
    pub fn prepare_for(
        &self,
        tool: Source,
        source: &str,
        additional: &str,
        target_project: Option<&str>,
        project_id: Option<&str>,
        ctx: Context,
    ) -> Result<Record> {
        let (from, to) = match tool {
            Source::Codex => ("codex", "deepseek"),
            Source::Harness => ("deepseek", "codex"),
        };
        let fingerprint = if tool == Source::Codex {
            serde_json::to_vec(&(from, source, &ctx.version, to, &ctx.cwd, additional))
        } else {
            serde_json::to_vec(&(
                from,
                source,
                &ctx.version,
                to,
                &ctx.cwd,
                project_id,
                additional,
            ))
        };
        let fp = digest(&fingerprint.map_err(|e| e.to_string())?);
        let id = format!("session-{}", Uuid::new_v4());
        let title = format!("{} · {}", ctx.title, id.get(id.len() - 12..).unwrap());
        let body = format!("[Session Take {id}]\n\n{}", ctx.text);
        self.0.execute("INSERT INTO handoffs(id,fingerprint,source_id,version,title,cwd,target_project,body,body_hash,stage,source_tool,project_id)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'prepared',?10,?11) ON CONFLICT(fingerprint) DO NOTHING",
            params![id,fp,source,ctx.version,title,ctx.cwd,target_project,body,digest(body.as_bytes()),from,project_id]).map_err(|e|e.to_string())?;
        let id: String = self
            .0
            .query_row("SELECT id FROM handoffs WHERE fingerprint=?1", [fp], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        self.get(&id)
    }
    pub fn get(&self, id: &str) -> Result<Record> {
        self.0
            .query_row(
                "SELECT id,title,cwd,target_project,body,stage,source_tool,project_id,native_id FROM handoffs WHERE id=?1",
                [id],
                |r| {
                    Ok(Record {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        cwd: r.get(2)?,
                        target_project: r.get(3)?,
                        body: r.get(4)?,
                        stage: r.get(5)?,
                        source: match r.get::<_, String>(6)?.as_str() { "codex" => Source::Codex, "deepseek" => Source::Harness, _ => return Err(rusqlite::Error::InvalidQuery) },
                        project_id: r.get(7)?,
                        native_id: r.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "operation_missing".into())
    }
    pub fn remember_native(&self, id: &str, native_id: &str) -> Result<()> {
        self.0
            .execute(
                "UPDATE handoffs SET native_id=?2 WHERE id=?1",
                params![id, native_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn delivered(&self, id: &str) -> Result<()> {
        self.0
            .execute(
                "UPDATE handoffs SET stage='delivered',error=NULL WHERE id=?1 AND stage='prepared'",
                [id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn queued(&self, id: &str) -> Result<()> {
        self.0
            .execute(
                "UPDATE handoffs SET stage='queued',error=NULL WHERE id=?1",
                [id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn completed(&self, id: &str, stage: &str) -> Result<()> {
        self.0
            .execute(
                "UPDATE handoffs SET stage=?2,body=NULL,error=NULL WHERE id=?1",
                [id, stage],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn failed(&self, id: &str, code: &str) -> Result<()> {
        self.0
            .execute(
                "UPDATE handoffs SET error=?1 WHERE id=?2",
                params![code, id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_restores_the_original_target_and_frozen_text() {
        let db = Records::open(Path::new(":memory:")).unwrap();
        let ctx = || Context {
            text: "context".into(),
            cwd: "/project".into(),
            version: "v1".into(),
            title: "Test".into(),
        };
        let first = db.prepare("source", "", None, ctx()).unwrap();
        let reverse = db
            .prepare_for(
                Source::Harness,
                "source",
                "",
                Some("/project"),
                Some("a"),
                ctx(),
            )
            .unwrap();
        let other_project = db
            .prepare_for(
                Source::Harness,
                "source",
                "",
                Some("/project"),
                Some("b"),
                ctx(),
            )
            .unwrap();
        assert_ne!(reverse.id, first.id);
        assert_ne!(reverse.id, other_project.id);
        let duplicate = db.prepare("source", "", None, ctx()).unwrap();
        assert_eq!(first.id, duplicate.id);
        assert_eq!(first.body, duplicate.body);
        db.delivered(&first.id).unwrap();
        db.failed(&first.id, "open").unwrap();
        assert_eq!(
            db.prepare("source", "", None, ctx()).unwrap().stage,
            "delivered"
        );
        db.completed(&first.id, "opened").unwrap();
        let done = db.get(&first.id).unwrap();
        assert!(done.body.is_none());
        assert_eq!(done.stage, "opened");
        assert_ne!(
            db.prepare("source", "new", None, ctx()).unwrap().id,
            first.id
        );
        let other_target = db
            .prepare(
                "source",
                "",
                Some("/target"),
                Context {
                    cwd: "/target".into(),
                    ..ctx()
                },
            )
            .unwrap();
        assert_ne!(other_target.id, first.id);
        assert_eq!(other_target.target_project.as_deref(), Some("/target"));
    }
}
