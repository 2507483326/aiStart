use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

use crate::db;
use crate::error::AppResult;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventView {
    pub id: i64,
    pub time: String,
    pub actor_kind: String,
    pub actor_name: Option<String>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub payload: Option<String>,
}

/// 审计事件只插不改；写入失败只丢弃该事件，不影响主流程。
pub fn log(
    actor_kind: &str,
    actor_name: Option<&str>,
    event_type: &str,
    target_kind: Option<&str>,
    target_id: Option<&str>,
    payload: Option<Value>,
) {
    let now = db::now_ms();
    let payload = payload.map(|value| value.to_string());
    let _ = db::with_conn(|connection| {
        connection.execute(
            "INSERT INTO events (event_time, actor_kind, actor_name, type, target_kind, target_id, payload, created_time, update_time) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?1, ?1)",
            params![now, actor_kind, actor_name, event_type, target_kind, target_id, payload],
        )?;
        Ok(())
    });
}

pub fn list(limit: usize) -> AppResult<Vec<EventView>> {
    db::with_conn(|connection| {
        let mut statement = connection.prepare(
            "SELECT event_id, event_time, actor_kind, actor_name, type, target_kind, target_id, payload \
             FROM events ORDER BY event_id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            let event_time: i64 = row.get(1)?;
            Ok(EventView {
                id: row.get(0)?,
                time: db::iso_from_ms(event_time),
                actor_kind: row.get(2)?,
                actor_name: row.get(3)?,
                event_type: row.get(4)?,
                target_kind: row.get(5)?,
                target_id: row.get(6)?,
                payload: row.get(7)?,
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    })
}
