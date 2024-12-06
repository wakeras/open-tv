use std::{collections::HashMap, sync::LazyLock};

use crate::log::log;
use crate::types::{CustomChannel, CustomChannelExtraData, ExportedGroup, Group, IdName};
use crate::{
    media_type, source_type,
    types::{Channel, ChannelHttpHeaders, Filters, Source},
    view_type,
};
use anyhow::{anyhow, Context, Result};
use directories::ProjectDirs;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, params_from_iter, OptionalExtension, Row, Transaction};
use rusqlite_migration::{Migrations, M};

const PAGE_SIZE: u8 = 36;
static CONN: LazyLock<Pool<SqliteConnectionManager>> = LazyLock::new(|| create_connection_pool());

pub fn get_conn() -> Result<PooledConnection<SqliteConnectionManager>> {
    CONN.try_get().context("No sqlite conns available")
}

fn create_connection_pool() -> Pool<SqliteConnectionManager> {
    let manager = SqliteConnectionManager::file(get_and_create_sqlite_db_path());
    r2d2::Pool::builder().max_size(20).build(manager).unwrap()
}

fn get_and_create_sqlite_db_path() -> String {
    let mut path = ProjectDirs::from("dev", "fredol", "open-tv")
        .unwrap()
        .data_dir()
        .to_owned();
    if !path.exists() {
        std::fs::create_dir_all(&path).unwrap();
    }
    path.push("db.sqlite");
    return path.to_string_lossy().to_string();
}

fn create_structure() -> Result<()> {
    let sql = get_conn()?;
    sql.execute_batch(
        r#"
CREATE TABLE "sources" (
  "id"          INTEGER PRIMARY KEY,
  "name"        varchar(100),
  "source_type" integer,
  "url"         varchar(500),
  "username"    varchar(100),
  "password"    varchar(100),
  "enabled"     integer DEFAULT 1
);

CREATE TABLE "channels" (
  "id" INTEGER PRIMARY KEY,
  "name" varchar(100),
  "image" varchar(500),
  "url" varchar(500),
  "media_type" integer,
  "source_id" integer,
  "favorite" integer,
  "series_id" integer,
  "group_id" integer,
  FOREIGN KEY (source_id) REFERENCES sources(id)
  FOREIGN KEY (group_id) REFERENCES groups(id)
);

CREATE TABLE "settings" (
  "key" VARCHAR(50) PRIMARY KEY,
  "value" VARCHAR(100)
);

CREATE TABLE "groups" (
  "id" INTEGER PRIMARY KEY,
  "name" varchar(100),
  "image" varchar(500),
  "source_id" integer,
  FOREIGN KEY (source_id) REFERENCES sources(id)
);

CREATE INDEX index_channel_name ON channels(name);
CREATE UNIQUE INDEX channels_unique ON channels(name, url);

CREATE UNIQUE INDEX index_source_name ON sources(name);
CREATE INDEX index_source_enabled ON sources(enabled);

CREATE UNIQUE INDEX index_group_unique ON groups(name, source_id);
CREATE INDEX index_group_name ON groups(name);

CREATE INDEX index_channel_source_id ON channels(source_id);
CREATE INDEX index_channel_favorite ON channels(favorite);
CREATE INDEX index_channel_series_id ON channels(series_id);
CREATE INDEX index_channel_group_id ON channels(group_id);
CREATE INDEX index_channel_media_type ON channels(media_type);

CREATE INDEX index_group_source_id ON groups(source_id);
"#,
    )?;
    Ok(())
}

fn structure_exists() -> Result<bool> {
    let sql = get_conn()?;
    let table_exists: bool = sql
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'channels' LIMIT 1",
            [],
            |row| row.get::<_, u8>(0),
        )
        .optional()?
        .is_some();
    Ok(table_exists)
}

pub fn delete_database() -> Result<()> {
    std::fs::remove_file(get_and_create_sqlite_db_path())?;
    std::process::exit(0);
}

pub fn create_or_initialize_db() -> Result<()> {
    if !structure_exists()? {
        create_structure()?;
    }
    apply_migrations()?;
    Ok(())
}

fn apply_migrations() -> Result<()> {
    let mut sql = get_conn()?;
    let migrations = Migrations::new(vec![
        M::up(r#"
            DROP INDEX IF EXISTS channels_unique;
            CREATE UNIQUE INDEX channels_unique ON channels(name, url, source_id);
            CREATE TABLE IF NOT EXISTS "channel_http_headers" (
                "id" INTEGER PRIMARY KEY,
                "channel_id" integer,
                "referrer" varchar(500),
                "user_agent" varchar(500),
                "http_origin" varchar(500),
                "ignore_ssl" integer DEFAULT 0,
                FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE
            );
            CREATE UNIQUE INDEX IF NOT EXISTS index_channel_http_headers_channel_id ON channel_http_headers(channel_id);
            ALTER TABLE sources ADD COLUMN use_tvg_id integer;
            UPDATE sources SET use_tvg_id = 1 WHERE source_type in (0,1);
        "#),
    ]);
    migrations.to_latest(&mut sql)?;
    Ok(())
}

pub fn drop_db() -> Result<()> {
    let sql = get_conn()?;
    sql.execute_batch(
        "DROP TABLE channels; DROP TABLE groups; DROP TABLE sources; DROP TABLE settings;",
    )?;
    Ok(())
}

pub fn create_or_find_source_by_name(tx: &Transaction, source: &Source) -> Result<i64> {
    let id: Option<i64> = tx
        .query_row(
            "SELECT id FROM sources WHERE name = ?1",
            params![source.name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        return Ok(id);
    }
    tx.execute(
    "INSERT INTO sources (name, source_type, url, username, password, use_tvg_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    params![source.name, source.source_type.clone() as u8, source.url, source.username, source.password, source.use_tvg_id],
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn insert_channel(tx: &Transaction, channel: Channel) -> Result<()> {
    tx.execute(
        r#" 
INSERT OR IGNORE INTO channels (name, group_id, image, url, source_id, media_type, series_id, favorite) 
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8); 
"#,
        params![
            channel.name,
            channel.group_id,
            channel.image,
            channel.url,
            channel.source_id,
            channel.media_type as u8,
            channel.series_id,
            channel.favorite
        ],
    )?;
    Ok(())
}

pub fn insert_channel_headers(tx: &Transaction, headers: ChannelHttpHeaders) -> Result<()> {
    tx.execute(
        r#"
INSERT OR IGNORE INTO channel_http_headers (channel_id, referrer, user_agent, http_origin) 
VALUES (?, ?, ?, ?); 
"#,
        params![
            headers.channel_id,
            headers.referrer,
            headers.user_agent,
            headers.http_origin
        ],
    )?;
    Ok(())
}

fn insert_group(
    tx: &Transaction,
    group: &str,
    image: &Option<String>,
    source_id: &i64,
) -> Result<i64> {
    let rows_changed = tx.execute(
        r#"
        INSERT OR IGNORE INTO groups (name, image, source_id) 
        VALUES (?1, ?2, ?3); 
        "#,
        params![group, &image, source_id],
    )?;
    if rows_changed == 0 {
        return Err(anyhow!("Failed to insert group: {}", group));
    }
    Ok(tx.last_insert_rowid())
}

pub fn set_channel_group_id(
    groups: &mut HashMap<String, i64>,
    channel: &mut Channel,
    tx: &Transaction,
    source_id: &i64,
) -> Result<()> {
    if channel.group.is_none() {
        return Ok(());
    }
    if !groups.contains_key(channel.group.as_ref().unwrap()) {
        let id = insert_group(
            tx,
            channel.group.as_ref().unwrap(),
            &channel.image,
            source_id,
        )?;
        groups.insert(channel.group.clone().unwrap(), id);
        channel.group_id = Some(id);
    } else {
        channel.group_id = groups
            .get(channel.group.as_ref().unwrap())
            .map(|x| x.to_owned());
    }
    Ok(())
}

pub fn get_channel_headers_by_id(id: i64) -> Result<Option<ChannelHttpHeaders>> {
    let sql = get_conn()?;
    let headers = sql
        .query_row(
            "SELECT * FROM channel_http_headers WHERE channel_id = ?",
            params![id],
            row_to_channel_headers,
        )
        .optional()?;
    Ok(headers)
}

fn row_to_channel_headers(row: &Row) -> Result<ChannelHttpHeaders, rusqlite::Error> {
    Ok(ChannelHttpHeaders {
        id: row.get("id")?,
        channel_id: row.get("channel_id")?,
        http_origin: row.get("http_origin")?,
        referrer: row.get("referrer")?,
        user_agent: row.get("user_agent")?,
        ignore_ssl: row.get("ignore_ssl")?,
    })
}

pub fn get_settings() -> Result<HashMap<String, String>> {
    let sql = get_conn()?;
    let map = sql
        .prepare("SELECT key, value FROM Settings")?
        .query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })?
        .filter_map(Result::ok)
        .collect();
    Ok(map)
}

pub fn update_settings(map: HashMap<String, String>) -> Result<()> {
    let mut sql: PooledConnection<SqliteConnectionManager> = get_conn()?;
    let tx = sql.transaction()?;
    for (key, value) in map {
        tx.execute(
            r#"
            INSERT INTO Settings (key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = ?2
            "#,
            params![key, value],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn search(filters: Filters) -> Result<Vec<Channel>> {
    if filters.view_type == view_type::CATEGORIES
        && filters.group_id.is_none()
        && filters.series_id.is_none()
    {
        return search_group(filters);
    }
    let sql = get_conn()?;
    let offset: u16 = filters.page as u16 * PAGE_SIZE as u16 - PAGE_SIZE as u16;
    let media_types = match filters.series_id.is_some() {
        true => vec![1],
        false => filters.media_types.clone().unwrap(),
    };
    let mut sql_query = format!(
        r#"
        SELECT * FROM CHANNELS
        WHERE name LIKE ?
        AND media_type IN ({})
		AND source_id IN ({})
        AND url IS NOT NULL"#,
        generate_placeholders(media_types.len()),
        generate_placeholders(filters.source_ids.len()),
    );
    let mut baked_params = 3;
    if filters.view_type == view_type::FAVORITES && filters.series_id.is_none() {
        sql_query += "\nAND favorite = 1";
    }
    if filters.series_id.is_some() {
        sql_query += &format!("\nAND series_id = ?");
        baked_params += 1;
    } else if filters.group_id.is_some() {
        sql_query += &format!("\nAND group_id = ?");
        baked_params += 1;
    }
    sql_query += "\nLIMIT ?, ?";
    let mut params: Vec<&dyn rusqlite::ToSql> =
        Vec::with_capacity(baked_params + media_types.len() + filters.source_ids.len());
    let query = to_sql_like(filters.query);
    params.push(&query);
    params.extend(to_to_sql(&media_types));
    params.extend(to_to_sql(&filters.source_ids));
    if let Some(ref series_id) = filters.series_id {
        params.push(series_id);
    } else if let Some(ref group) = filters.group_id {
        params.push(group);
    }
    params.push(&offset);
    params.push(&PAGE_SIZE);
    let channels: Vec<Channel> = sql
        .prepare(&sql_query)?
        .query_map(params_from_iter(params), row_to_channel)?
        .filter_map(Result::ok)
        .collect();
    Ok(channels)
}

fn to_to_sql<T: rusqlite::ToSql>(values: &[T]) -> Vec<&dyn rusqlite::ToSql> {
    values.iter().map(|x| x as &dyn rusqlite::ToSql).collect()
}

fn generate_placeholders(size: usize) -> String {
    std::iter::repeat("?")
        .take(size)
        .collect::<Vec<_>>()
        .join(",")
}

pub fn series_has_episodes(series_id: i64) -> Result<bool> {
    let sql = get_conn()?;
    let series_exists = sql
        .query_row(
            r#"
      SELECT 1 
      FROM channels
      WHERE series_id = ?
      LIMIT 1
    "#,
            params![series_id],
            |row| row.get::<_, u8>(0),
        )
        .optional()?
        .is_some();
    Ok(series_exists)
}

fn to_sql_like(query: Option<String>) -> String {
    query.map(|x| format!("%{x}%")).unwrap_or("%".to_string())
}

pub fn search_group(filters: Filters) -> Result<Vec<Channel>> {
    let sql = get_conn()?;
    let offset = filters.page * PAGE_SIZE - PAGE_SIZE;
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(3 + filters.source_ids.len());
    let sql_query = format!(
        r#"
        SELECT *
        FROM groups
        WHERE name like ?
        AND source_id in ({})
        LIMIT ?, ?
    "#,
        generate_placeholders(filters.source_ids.len())
    );
    let query = to_sql_like(filters.query);
    params.push(&query);
    params.extend(to_to_sql(&filters.source_ids));
    params.push(&offset);
    params.push(&PAGE_SIZE);
    let channels: Vec<Channel> = sql
        .prepare(&sql_query)?
        .query_map(params_from_iter(params), row_to_group)?
        .filter_map(Result::ok)
        .collect();
    Ok(channels)
}

fn row_to_group(row: &Row) -> std::result::Result<Channel, rusqlite::Error> {
    let channel = Channel {
        id: row.get("id")?,
        name: row.get("name")?,
        group: None,
        image: row.get("image")?,
        media_type: media_type::GROUP,
        url: None,
        series_id: None,
        group_id: None,
        favorite: false,
        source_id: row.get("source_id")?,
    };
    Ok(channel)
}

fn row_to_channel(row: &Row) -> std::result::Result<Channel, rusqlite::Error> {
    let channel = Channel {
        id: row.get("id")?,
        name: row.get("name")?,
        group_id: row.get("group_id")?,
        image: row.get("image")?,
        media_type: row.get("media_type")?,
        source_id: row.get("source_id")?,
        url: row.get("url")?,
        favorite: row.get("favorite")?,
        series_id: None,
        group: None,
    };
    Ok(channel)
}

pub fn delete_channels_by_source(source_id: i64) -> Result<()> {
    let sql = get_conn()?;
    sql.execute(
        r#"
        DELETE FROM channels
        WHERE source_id = ?
        AND favorite = 0;
    "#,
        params![source_id.to_string()],
    )?;
    Ok(())
}

pub fn delete_groups_by_source(source_id: i64) -> Result<()> {
    let sql = get_conn()?;
    sql.execute(
        r#"
        DELETE FROM groups
        WHERE source_id = ?
        AND ID not in (
            SELECT group_id 
            FROM channels
            WHERE favorite = 1
        )
    "#,
        params!(source_id),
    )?;
    Ok(())
}

pub fn delete_source(id: i64) -> Result<()> {
    let sql = get_conn()?;
    sql.execute(
        r#"
        DELETE FROM channels
        WHERE source_id = ?;
    "#,
        params![id],
    )?;
    sql.execute(
        r#"
        DELETE FROM groups
        WHERE source_id = ?;
    "#,
        params![id],
    )?;
    let count = sql.execute(
        r#"
        DELETE FROM sources
        WHERE id = ?;
    "#,
        params![id],
    )?;
    if count != 1 {
        return Err(anyhow!("No sources were deleted"));
    }
    Ok(())
}

pub fn get_channel_count_by_source(id: i64) -> Result<u64> {
    let sql = get_conn()?;
    let count = sql.query_row(
        "SELECT COUNT(*) FROM channels WHERE source_id = ?",
        params![id],
        |row| row.get::<_, u64>(0),
    )?;
    Ok(count)
}

pub fn source_name_exists(name: &str) -> Result<bool> {
    let sql = get_conn()?;
    Ok(sql
        .query_row(
            r#"
    SELECT 1 
    FROM sources 
    WHERE name = ?1
    "#,
            [name],
            |row| row.get::<_, u8>(0),
        )
        .optional()?
        .is_some())
}

pub fn favorite_channel(channel_id: i64, favorite: bool) -> Result<()> {
    let sql = get_conn()?;
    sql.execute(
        r#"
        UPDATE channels
        SET favorite = ?1
        WHERE id = ?2
    "#,
        params![favorite, channel_id],
    )?;
    Ok(())
}

pub fn get_sources() -> Result<Vec<Source>> {
    let sql = get_conn()?;
    let sources: Vec<Source> = sql
        .prepare("SELECT * FROM sources")?
        .query_map([], row_to_source)?
        .filter_map(Result::ok)
        .collect();
    Ok(sources)
}

pub fn get_enabled_sources() -> Result<Vec<Source>> {
    let sql = get_conn()?;
    let sources: Vec<Source> = sql
        .prepare("SELECT * FROM sources WHERE enabled = 1")?
        .query_map([], row_to_source)?
        .filter_map(Result::ok)
        .collect();
    Ok(sources)
}

fn row_to_source(row: &Row) -> std::result::Result<Source, rusqlite::Error> {
    Ok(Source {
        id: row.get("id")?,
        name: row.get("name")?,
        username: row.get("username")?,
        password: row.get("password")?,
        url: row.get("url")?,
        source_type: row.get("source_type")?,
        url_origin: None,
        enabled: row.get("enabled")?,
        use_tvg_id: row.get("use_tvg_id")?
    })
}

pub fn get_source_from_series_id(series_id: i64) -> Result<Source> {
    let sql = get_conn()?;
    Ok(sql.query_row(
        r#"
    SELECT * FROM sources where id = (
        SELECT source_id FROM channels WHERE url = ?
    )"#,
        [series_id],
        row_to_source,
    )?)
}

pub fn set_source_enabled(value: bool, source_id: i64) -> Result<()> {
    let sql = get_conn()?;
    sql.execute(
        r#"
        UPDATE sources
        SET enabled = ?
        WHERE id = ?
    "#,
        params![value, source_id],
    )?;
    Ok(())
}

pub fn add_custom_channel(tx: &Transaction, channel: CustomChannel) -> Result<()> {
    insert_channel(tx, channel.data)?;
    if let Some(mut headers) = channel.headers {
        if channel_headers_empty(&headers) {
            return Ok(());
        }
        headers.channel_id = Some(tx.last_insert_rowid());
        insert_channel_headers(tx, headers)?;
    }
    Ok(())
}

fn channel_headers_empty(headers: &ChannelHttpHeaders) -> bool {
    return headers.ignore_ssl.is_none()
        && headers.http_origin.is_none()
        && headers.referrer.is_none()
        && headers.user_agent.is_none();
}

pub fn get_custom_source(name: String) -> Source {
    Source {
        id: None,
        name: name.to_string(),
        enabled: true,
        username: None,
        password: None,
        source_type: source_type::CUSTOM,
        url: None,
        url_origin: None,
        use_tvg_id: None
    }
}

pub fn edit_custom_channel(channel: CustomChannel) -> Result<()> {
    let mut sql = get_conn()?;
    let tx = sql.transaction()?;
    match edit_custom_channel_tx(channel, &tx) {
        Ok(_) => {
            tx.commit()?;
            Ok(())
        }
        Err(e) => {
            tx.rollback().unwrap_or_else(|e| log(format!("{:?}", e)));
            return Err(e);
        }
    }
}

fn edit_custom_channel_tx(channel: CustomChannel, tx: &Transaction) -> Result<()> {
    tx.execute(
        r#"
        UPDATE channels
        SET name = ?, image = ?, url = ?, media_type = ?, group_id = ?
        WHERE id = ?
    "#,
        params![
            channel.data.name,
            channel.data.image,
            channel.data.url,
            channel.data.media_type,
            channel.data.group_id,
            channel.data.id
        ],
    )?;
    if let Some(mut headers) = channel.headers {
        headers.channel_id = channel.data.id;
        tx.execute(
            r#"
            INSERT INTO channel_http_headers (referrer, user_agent, http_origin, ignore_ssl, channel_id)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(channel_id) DO UPDATE SET 
                referrer = ?1, 
                user_agent = ?2, 
                http_origin = ?3, 
                ignore_ssl = ?4
        "#,
            params![
                headers.referrer,
                headers.user_agent,
                headers.http_origin,
                headers.ignore_ssl,
                headers.channel_id
            ],
        )?;
    } else {
        tx.execute(
            "DELETE FROM channel_http_headers WHERE channel_id = ?",
            params![channel.data.id],
        )?;
    }
    Ok(())
}

pub fn delete_custom_channel(id: i64) -> Result<()> {
    let sql = get_conn()?;
    sql.execute("DELETE FROM channels WHERE id = ?", params![id])?;
    Ok(())
}

pub fn group_exists(name: &str, source_id: i64) -> Result<bool> {
    let sql = get_conn()?;
    Ok(sql
        .query_row(
            r#"
            SELECT 1 
            FROM groups 
            WHERE name = ? AND source_id = ?
        "#,
            params![name, source_id],
            |row| row.get::<_, u8>(0),
        )
        .optional()?
        .is_some())
}

pub fn channel_exists(name: &str, url: &str, source_id: i64) -> Result<bool> {
    let sql = get_conn()?;
    Ok(sql
        .query_row(
            r#"
            SELECT 1 
            FROM channels 
            WHERE name = ? AND source_id = ? AND url = ?
        "#,
            params![name, source_id, url],
            |row| row.get::<_, u8>(0),
        )
        .optional()?
        .is_some())
}

pub fn add_custom_group(tx: &Transaction, group: Group) -> Result<i64> {
    tx.execute(
        r#"
        INSERT INTO groups (name, image, source_id) 
        VALUES (?, ?, ?)
    "#,
        params!(group.name, group.image, group.source_id),
    )?;
    Ok(tx.last_insert_rowid())
}

pub fn group_auto_complete(query: Option<String>, source_id: i64) -> Result<Vec<IdName>> {
    let sql = get_conn()?;
    let groups = sql
        .prepare(
            r#"
        SELECT id, name 
        FROM groups
        WHERE name LIKE ?
        AND source_id = ?
    "#,
        )?
        .query_map(params![to_sql_like(query), source_id], row_to_id_name)?
        .filter_map(Result::ok)
        .collect();
    Ok(groups)
}

fn row_to_id_name(row: &Row) -> Result<IdName, rusqlite::Error> {
    Ok(IdName {
        id: row.get("id")?,
        name: row.get("name")?,
    })
}

pub fn edit_custom_group(group: Group) -> Result<()> {
    let sql = get_conn()?;
    sql.execute(
        r#"
        UPDATE groups 
        SET name = ?, image = ?
        WHERE id = ?
    "#,
        params![group.name, group.image, group.id],
    )?;
    Ok(())
}

fn get_group_by_id(id: i64) -> Result<Option<Group>> {
    let sql = get_conn()?;
    let group: Option<Group> = sql
        .query_row(
            "SELECT * FROM groups WHERE id = ?",
            params![id],
            row_to_custom_group,
        )
        .optional()?;
    Ok(group)
}

fn row_to_custom_group(row: &Row) -> Result<Group, rusqlite::Error> {
    Ok(Group {
        id: row.get("id")?,
        name: row.get("name")?,
        image: row.get("image")?,
        source_id: row.get("source_id")?,
    })
}

pub fn get_custom_channel_extra_data(
    id: i64,
    group_id: Option<i64>,
) -> Result<CustomChannelExtraData> {
    Ok(CustomChannelExtraData {
        headers: get_channel_headers_by_id(id)?,
        group: match group_id {
            None => None,
            Some(group) => get_group_by_id(group)?,
        },
    })
}

pub fn delete_custom_group(id: i64, new_id: Option<i64>, do_channels_update: bool) -> Result<()> {
    let sql = get_conn()?;
    if do_channels_update {
        sql.execute(
            r#"
        UPDATE channels
        SET group_id = ?
        WHERE group_id = ?
    "#,
            params![new_id, id],
        )?;
    }
    sql.execute(
        r#"
        DELETE FROM groups 
        WHERE id = ?
    "#,
        params![id],
    )?;
    Ok(())
}

pub fn group_not_empty(id: i64) -> Result<bool> {
    let sql = get_conn()?;
    Ok(sql
        .query_row(
            r#"
                SELECT 1
                FROM channels
                WHERE group_id = ?
            "#,
            params![id],
            |row| row.get::<_, u8>(0),
        )
        .optional()?
        .is_some())
}

pub fn get_custom_channels(group_id: Option<i64>, source_id: i64) -> Result<Vec<CustomChannel>> {
    let sql = get_conn()?;
    let mut sql_query = r#"
        SELECT c.name, c.image, c.url, c.media_type, ch.referrer, ch.user_agent, ch.http_origin, ch.ignore_ssl
        FROM channels c
        LEFT JOIN channel_http_headers ch on ch.channel_id = c.id
        WHERE source_id = ?
    "#.to_string();
    let mut params: Vec<i64> = Vec::with_capacity(2);
    params.push(source_id);
    if let Some(id) = group_id {
        sql_query.push_str("\nAND group_id = ?");
        params.push(id);
    } else {
        sql_query.push_str("\nAND group_id IS NULL");
    }
    let result = sql
        .prepare(&sql_query)?
        .query_map(params_from_iter(params), row_to_custom_channel)?
        .filter_map(Result::ok)
        .collect();
    Ok(result)
}

fn row_to_custom_channel(row: &Row) -> Result<CustomChannel, rusqlite::Error> {
    Ok(CustomChannel {
        data: Channel {
            name: row.get("name")?,
            image: row.get("image")?,
            url: row.get("url")?,
            media_type: row.get("media_type")?,
            favorite: false,
            group_id: None,
            group: None,
            id: None,
            series_id: None,
            source_id: None,
        },
        headers: Some(ChannelHttpHeaders {
            http_origin: row.get("http_origin")?,
            ignore_ssl: row.get("ignore_ssl")?,
            referrer: row.get("referrer")?,
            user_agent: row.get("user_agent")?,
            channel_id: None,
            id: None,
        }),
    })
}

fn get_groups_by_source_id(id: i64) -> Result<Vec<Group>> {
    let sql = get_conn()?;
    let result = sql
        .prepare(
            r#"
        SELECT * 
        FROM groups 
        WHERE source_id = ?
    "#,
        )?
        .query_map(params![id], row_to_custom_group)?
        .filter_map(Result::ok)
        .collect();
    Ok(result)
}

pub fn get_custom_groups(source_id: i64) -> Result<Vec<ExportedGroup>> {
    let groups = get_groups_by_source_id(source_id)?;
    let mut export: Vec<ExportedGroup> = Vec::new();
    for group in groups {
        export.push(ExportedGroup {
            group: Group {
                name: group.name,
                image: group.image,
                source_id: None,
                id: None,
            },
            channels: get_custom_channels(group.id, source_id)?,
        });
    }
    Ok(export)
}

pub fn do_tx<F, T>(f: F) -> Result<T>
where
    F: FnOnce(&Transaction) -> Result<T>,
{
    let mut sql = get_conn()?;
    let tx = sql.transaction()?;
    let result = f(&tx)?;
    tx.commit()?;
    Ok(result)
}

#[cfg(test)]
mod test_sql {
    use std::collections::HashMap;

    use crate::{
        media_type,
        settings::{RECORDING_PATH, USE_STREAM_CACHING},
        sql::{create_structure, drop_db, structure_exists},
        types::Filters,
        view_type,
    };

    use super::{get_sources, search, update_settings};

    #[test]
    fn test_structure_exists() {
        drop_db().unwrap();
        assert_eq!(structure_exists().unwrap(), false);
        create_structure().unwrap();
        assert_eq!(structure_exists().unwrap(), true);
    }
    #[test]
    fn test_update_settings() {
        drop_db().unwrap_or_default();
        create_structure().unwrap();
        let mut map: HashMap<String, String> = HashMap::with_capacity(3);
        map.insert(USE_STREAM_CACHING.to_string(), true.to_string());
        map.insert(RECORDING_PATH.to_string(), "somePath".to_string());
        update_settings(map.clone()).unwrap();
        update_settings(map).unwrap();
    }

    #[test]
    fn test_search() {
        let results = search(Filters {
            media_types: Some(vec![media_type::LIVESTREAM, media_type::MOVIE]),
            page: 1,
            query: Some("Fra".to_string()),
            source_ids: get_sources()
                .unwrap()
                .iter()
                .map(|x| x.id.unwrap())
                .collect(),
            view_type: view_type::ALL,
            group_id: None,
            series_id: None,
        })
        .unwrap();
        println!("{:?}\n\n", results);
        println!("{}", results.len());
    }

    #[test]
    fn test_drop_db() {
        drop_db().unwrap();
    }

    #[test]
    fn test_search_group() {
        let results = search(Filters {
            media_types: None,
            page: 1,
            query: Some("Fra".to_string()),
            source_ids: get_sources()
                .unwrap()
                .iter()
                .map(|x| x.id.unwrap())
                .collect(),
            view_type: view_type::CATEGORIES,
            group_id: None,
            series_id: None,
        })
        .unwrap();
        println!("{:?}\n\n", results);
        println!("{}", results.len());
    }

    #[test]
    fn test_get_sources() {
        let results = get_sources().unwrap();
        println!("{:?}", results);
    }
}
