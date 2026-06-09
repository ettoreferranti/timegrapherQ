//! Local-first persistence: settings plus a SQLite store for the watch
//! collection and measurement history.
//!
//! The store lives in a user-chosen **data directory** (so it can sit inside an
//! iCloud/Dropbox folder); the data dir holds `timegrapherq.sqlite`, a
//! `recordings/` folder for optional saved clips, and a self-describing
//! `README.txt`. Settings (including the data-dir path) live separately in the
//! OS config directory so the app can find the data dir on launch.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Default recording/clip length offered in the UI.
pub const DEFAULT_CLIP_SECONDS: f64 = 20.0;

/// Persisted application settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Absolute path to the data directory (DB + recordings).
    pub data_dir: String,
    /// Default recording duration in seconds.
    pub default_clip_seconds: f64,
}

impl Settings {
    /// Load settings from `config_dir/settings.json`, falling back to defaults
    /// (data dir = `default_data_dir`).
    pub fn load_or_default(config_dir: &Path, default_data_dir: &Path) -> Settings {
        let path = config_dir.join("settings.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut s) = serde_json::from_str::<Settings>(&text) {
                if !s.default_clip_seconds.is_finite() || s.default_clip_seconds <= 0.0 {
                    s.default_clip_seconds = DEFAULT_CLIP_SECONDS;
                }
                return s;
            }
        }
        Settings {
            data_dir: default_data_dir.to_string_lossy().into_owned(),
            default_clip_seconds: DEFAULT_CLIP_SECONDS,
        }
    }

    /// Persist settings to `config_dir/settings.json`.
    pub fn save(&self, config_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(config_dir.join("settings.json"), json).map_err(|e| e.to_string())
    }
}

/// A watch in the collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Watch {
    pub id: String,
    pub name: String,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub calibre: Option<String>,
    pub reference: Option<String>,
    pub serial: Option<String>,
    pub lift_angle: f64,
    pub bph: u32,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Fields supplied by the UI when creating/updating a watch.
#[derive(Debug, Clone, Deserialize)]
pub struct WatchInput {
    pub name: String,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub calibre: Option<String>,
    pub reference: Option<String>,
    pub serial: Option<String>,
    pub lift_angle: f64,
    pub bph: u32,
    pub notes: Option<String>,
}

/// A saved measurement (test) linked to a watch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Test {
    pub id: String,
    pub watch_id: String,
    pub measured_at: String,
    pub position: Option<String>,
    pub rate_s_per_day: Option<f64>,
    pub beat_error_ms: Option<f64>,
    pub amplitude_deg: Option<f64>,
    pub bph_used: u32,
    pub lift_angle_used: f64,
    pub temperature_c: Option<f64>,
    pub power_state: Option<String>,
    pub device_name: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub clip_seconds: Option<f64>,
    pub quality: Option<f64>,
    pub beats_used: Option<u32>,
    pub audio_path: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
}

/// Fields supplied by the UI when saving a test.
#[derive(Debug, Clone, Deserialize)]
pub struct TestInput {
    pub position: Option<String>,
    pub rate_s_per_day: Option<f64>,
    pub beat_error_ms: Option<f64>,
    pub amplitude_deg: Option<f64>,
    pub bph_used: u32,
    pub lift_angle_used: f64,
    pub temperature_c: Option<f64>,
    pub power_state: Option<String>,
    pub device_name: Option<String>,
    pub sample_rate_hz: Option<u32>,
    pub clip_seconds: Option<f64>,
    pub quality: Option<f64>,
    pub beats_used: Option<u32>,
    /// If set, this WAV is copied into the data dir's `recordings/` and linked.
    pub source_audio_path: Option<String>,
    pub notes: Option<String>,
}

/// Editable metadata for an existing test (conditions and notes).
#[derive(Debug, Clone, Deserialize)]
pub struct TestEdit {
    pub position: Option<String>,
    pub temperature_c: Option<f64>,
    pub power_state: Option<String>,
    pub notes: Option<String>,
}

const SCHEMA_V1: &str = r#"
CREATE TABLE watch (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  brand         TEXT,
  model         TEXT,
  calibre       TEXT,
  reference     TEXT,
  serial        TEXT,
  lift_angle    REAL NOT NULL DEFAULT 52.0,
  bph           INTEGER NOT NULL DEFAULT 28800,
  notes         TEXT,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);

CREATE TABLE test (
  id              TEXT PRIMARY KEY,
  watch_id        TEXT NOT NULL REFERENCES watch(id) ON DELETE CASCADE,
  measured_at     TEXT NOT NULL,
  position        TEXT,
  rate_s_per_day  REAL,
  beat_error_ms   REAL,
  amplitude_deg   REAL,
  bph_used        INTEGER NOT NULL,
  lift_angle_used REAL NOT NULL,
  temperature_c   REAL,
  power_state     TEXT,
  device_name     TEXT,
  sample_rate_hz  INTEGER,
  clip_seconds    REAL,
  quality         REAL,
  beats_used      INTEGER,
  audio_path      TEXT,
  notes           TEXT,
  created_at      TEXT NOT NULL
);

CREATE INDEX idx_test_watch ON test(watch_id, measured_at);
"#;

const DATA_DIR_README: &str = "\
This folder holds your TimegrapherQ data.

  timegrapherq.sqlite  - your watch collection and measurement history (SQLite)
  recordings/          - optional saved audio clips (WAV), referenced by tests

You can move this folder and point TimegrapherQ at the new location in Settings.
Do NOT run two TimegrapherQ instances against this folder at the same time
(e.g. on two machines via a synced folder) - that can corrupt the database.
";

/// The SQLite-backed store, opened against a data directory.
pub struct Store {
    conn: Connection,
    data_dir: PathBuf,
}

impl Store {
    /// Open (creating if needed) the store in `data_dir`, running migrations.
    pub fn open(data_dir: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(data_dir.join("recordings")).map_err(|e| e.to_string())?;
        let _ = std::fs::write(data_dir.join("README.txt"), DATA_DIR_README);

        let conn = Connection::open(data_dir.join("timegrapherq.sqlite")).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        migrate(&conn)?;
        Ok(Store {
            conn,
            data_dir: data_dir.to_path_buf(),
        })
    }

    pub fn list_watches(&self) -> Result<Vec<Watch>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id,name,brand,model,calibre,reference,serial,lift_angle,bph,notes,\
                 created_at,updated_at FROM watch ORDER BY name COLLATE NOCASE",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], row_to_watch)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn create_watch(&self, input: &WatchInput) -> Result<Watch, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();
        self.conn
            .execute(
                "INSERT INTO watch (id,name,brand,model,calibre,reference,serial,lift_angle,bph,\
                 notes,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    id,
                    input.name,
                    input.brand,
                    input.model,
                    input.calibre,
                    input.reference,
                    input.serial,
                    input.lift_angle,
                    input.bph as i64,
                    input.notes,
                    now,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
        self.get_watch(&id)
    }

    pub fn update_watch(&self, id: &str, input: &WatchInput) -> Result<Watch, String> {
        let n = self
            .conn
            .execute(
                "UPDATE watch SET name=?2,brand=?3,model=?4,calibre=?5,reference=?6,serial=?7,\
                 lift_angle=?8,bph=?9,notes=?10,updated_at=?11 WHERE id=?1",
                params![
                    id,
                    input.name,
                    input.brand,
                    input.model,
                    input.calibre,
                    input.reference,
                    input.serial,
                    input.lift_angle,
                    input.bph as i64,
                    input.notes,
                    now_iso(),
                ],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("watch not found: {id}"));
        }
        self.get_watch(id)
    }

    pub fn delete_watch(&self, id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM watch WHERE id=?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_watch(&self, id: &str) -> Result<Watch, String> {
        self.conn
            .query_row(
                "SELECT id,name,brand,model,calibre,reference,serial,lift_angle,bph,notes,\
                 created_at,updated_at FROM watch WHERE id=?1",
                params![id],
                row_to_watch,
            )
            .map_err(|e| e.to_string())
    }

    pub fn list_tests(&self, watch_id: &str) -> Result<Vec<Test>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id,watch_id,measured_at,position,rate_s_per_day,beat_error_ms,amplitude_deg,\
                 bph_used,lift_angle_used,temperature_c,power_state,device_name,sample_rate_hz,\
                 clip_seconds,quality,beats_used,audio_path,notes,created_at \
                 FROM test WHERE watch_id=?1 ORDER BY measured_at DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![watch_id], row_to_test)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn create_test(&self, watch_id: &str, input: &TestInput) -> Result<Test, String> {
        // Ensure the watch exists (also gives a friendly error).
        self.get_watch(watch_id)?;

        let id = uuid::Uuid::new_v4().to_string();
        let now = now_iso();

        // Optionally retain the recorded clip alongside the test.
        let audio_path = match &input.source_audio_path {
            Some(src) if !src.is_empty() => {
                let rel = format!("recordings/{id}.wav");
                std::fs::copy(src, self.data_dir.join(&rel))
                    .map_err(|e| format!("could not save clip: {e}"))?;
                Some(rel)
            }
            _ => None,
        };

        self.conn
            .execute(
                "INSERT INTO test (id,watch_id,measured_at,position,rate_s_per_day,beat_error_ms,\
                 amplitude_deg,bph_used,lift_angle_used,temperature_c,power_state,device_name,\
                 sample_rate_hz,clip_seconds,quality,beats_used,audio_path,notes,created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![
                    id,
                    watch_id,
                    now,
                    input.position,
                    input.rate_s_per_day,
                    input.beat_error_ms,
                    input.amplitude_deg,
                    input.bph_used as i64,
                    input.lift_angle_used,
                    input.temperature_c,
                    input.power_state,
                    input.device_name,
                    input.sample_rate_hz.map(|v| v as i64),
                    input.clip_seconds,
                    input.quality,
                    input.beats_used.map(|v| v as i64),
                    audio_path,
                    input.notes,
                    now,
                ],
            )
            .map_err(|e| e.to_string())?;
        self.get_test(&id)
    }

    pub fn update_test(&self, id: &str, edit: &TestEdit) -> Result<Test, String> {
        let n = self
            .conn
            .execute(
                "UPDATE test SET position=?2,temperature_c=?3,power_state=?4,notes=?5 WHERE id=?1",
                params![
                    id,
                    edit.position,
                    edit.temperature_c,
                    edit.power_state,
                    edit.notes,
                ],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err(format!("test not found: {id}"));
        }
        self.get_test(id)
    }

    pub fn delete_test(&self, id: &str) -> Result<(), String> {
        // Remove the linked recording if present.
        if let Ok(t) = self.get_test(id) {
            if let Some(rel) = t.audio_path {
                let _ = std::fs::remove_file(self.data_dir.join(rel));
            }
        }
        self.conn
            .execute("DELETE FROM test WHERE id=?1", params![id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_test(&self, id: &str) -> Result<Test, String> {
        self.conn
            .query_row(
                "SELECT id,watch_id,measured_at,position,rate_s_per_day,beat_error_ms,amplitude_deg,\
                 bph_used,lift_angle_used,temperature_c,power_state,device_name,sample_rate_hz,\
                 clip_seconds,quality,beats_used,audio_path,notes,created_at \
                 FROM test WHERE id=?1",
                params![id],
                row_to_test,
            )
            .map_err(|e| e.to_string())
    }
}

fn migrate(conn: &Connection) -> Result<(), String> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version < 1 {
        conn.execute_batch(SCHEMA_V1).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "user_version", 1)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn row_to_watch(row: &rusqlite::Row) -> rusqlite::Result<Watch> {
    Ok(Watch {
        id: row.get(0)?,
        name: row.get(1)?,
        brand: row.get(2)?,
        model: row.get(3)?,
        calibre: row.get(4)?,
        reference: row.get(5)?,
        serial: row.get(6)?,
        lift_angle: row.get(7)?,
        bph: row.get::<_, i64>(8)? as u32,
        notes: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn row_to_test(row: &rusqlite::Row) -> rusqlite::Result<Test> {
    Ok(Test {
        id: row.get(0)?,
        watch_id: row.get(1)?,
        measured_at: row.get(2)?,
        position: row.get(3)?,
        rate_s_per_day: row.get(4)?,
        beat_error_ms: row.get(5)?,
        amplitude_deg: row.get(6)?,
        bph_used: row.get::<_, i64>(7)? as u32,
        lift_angle_used: row.get(8)?,
        temperature_c: row.get(9)?,
        power_state: row.get(10)?,
        device_name: row.get(11)?,
        sample_rate_hz: row.get::<_, Option<i64>>(12)?.map(|v| v as u32),
        clip_seconds: row.get(13)?,
        quality: row.get(14)?,
        beats_used: row.get::<_, Option<i64>>(15)?.map(|v| v as u32),
        audio_path: row.get(16)?,
        notes: row.get(17)?,
        created_at: row.get(18)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("tgq-test-{}", uuid::Uuid::new_v4()));
        (Store::open(&dir).expect("open store"), dir)
    }

    fn sample_watch() -> WatchInput {
        WatchInput {
            name: "Speedmaster".to_string(),
            brand: Some("Omega".to_string()),
            model: None,
            calibre: Some("1861".to_string()),
            reference: None,
            serial: None,
            lift_angle: 52.0,
            bph: 21_600,
            notes: None,
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = std::env::temp_dir().join(format!("tgq-test-{}", uuid::Uuid::new_v4()));
        {
            let s = Store::open(&dir).unwrap();
            s.create_watch(&sample_watch()).unwrap();
        }
        // Reopen: should not error and should keep the data.
        let s = Store::open(&dir).unwrap();
        assert_eq!(s.list_watches().unwrap().len(), 1);
        assert!(dir.join("README.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watch_crud_round_trip() {
        let (s, dir) = temp_store();
        let w = s.create_watch(&sample_watch()).unwrap();
        assert_eq!(w.bph, 21_600);

        let mut edit = sample_watch();
        edit.name = "Speedmaster Pro".to_string();
        edit.bph = 28_800;
        let updated = s.update_watch(&w.id, &edit).unwrap();
        assert_eq!(updated.name, "Speedmaster Pro");
        assert_eq!(updated.bph, 28_800);

        assert_eq!(s.list_watches().unwrap().len(), 1);
        s.delete_watch(&w.id).unwrap();
        assert_eq!(s.list_watches().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tests_link_to_watch_and_cascade_delete() {
        let (s, dir) = temp_store();
        let w = s.create_watch(&sample_watch()).unwrap();

        let input = TestInput {
            position: Some("DU".to_string()),
            rate_s_per_day: Some(3.2),
            beat_error_ms: Some(0.4),
            amplitude_deg: Some(285.0),
            bph_used: 21_600,
            lift_angle_used: 52.0,
            temperature_c: Some(21.0),
            power_state: Some("full wind".to_string()),
            device_name: Some("Piezo".to_string()),
            sample_rate_hz: Some(48_000),
            clip_seconds: Some(20.0),
            quality: Some(0.92),
            beats_used: Some(150),
            source_audio_path: None,
            notes: Some("after service".to_string()),
        };
        let t = s.create_test(&w.id, &input).unwrap();
        assert_eq!(t.watch_id, w.id);
        assert_eq!(s.list_tests(&w.id).unwrap().len(), 1);

        // Edit conditions.
        let edited = s
            .update_test(
                &t.id,
                &TestEdit {
                    position: Some("CD".to_string()),
                    temperature_c: Some(25.0),
                    power_state: None,
                    notes: Some("re-measured".to_string()),
                },
            )
            .unwrap();
        assert_eq!(edited.position.as_deref(), Some("CD"));

        // Deleting the watch cascades to its tests.
        s.delete_watch(&w.id).unwrap();
        assert_eq!(s.list_tests(&w.id).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_round_trip() {
        let cfg = std::env::temp_dir().join(format!("tgq-cfg-{}", uuid::Uuid::new_v4()));
        let data = std::env::temp_dir().join("tgq-data-default");
        let s = Settings::load_or_default(&cfg, &data);
        assert_eq!(s.default_clip_seconds, DEFAULT_CLIP_SECONDS);
        let custom = Settings {
            data_dir: "/tmp/my-watches".to_string(),
            default_clip_seconds: 30.0,
        };
        custom.save(&cfg).unwrap();
        let loaded = Settings::load_or_default(&cfg, &data);
        assert_eq!(loaded.data_dir, "/tmp/my-watches");
        assert_eq!(loaded.default_clip_seconds, 30.0);
        let _ = std::fs::remove_dir_all(&cfg);
    }
}
