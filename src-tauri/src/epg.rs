use std::{
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, Mutex,
    },
    thread,
};

use anyhow::{Context, Result};
use chrono::Local;
use tauri::{AppHandle, State};
use tauri_plugin_notification::NotificationExt;

use crate::{
    log, sql,
    types::{AppState, EPGNotify},
    utils,
};

pub fn poll(mut to_watch: Vec<EPGNotify>, stop: Arc<AtomicBool>, app: AppHandle) -> Result<()> {
    while stop.load(Relaxed) && !to_watch.is_empty() {
        to_watch.retain(|epg| {
            let is_timestamp_over = match is_timestamp_over(epg.start_timestamp) {
                Ok(v) => v,
                Err(e) => {
                    log::log(format!("{:?}", e));
                    return false;
                }
            };
            if is_timestamp_over {
                match notify(epg, &app).context("Failed to notify EPG") {
                    Ok(_) => {}
                    Err(e) => log::log(format!("{:?}", e)),
                }
                return false;
            }
            return true;
        });
    }
    Ok(())
}

fn notify(epg: &EPGNotify, app: &AppHandle) -> Result<()> {
    app.notification()
        .builder()
        .title(format!("LIVE: {}", epg.title))
        .body(format!("Watch on {}", epg.channel_name))
        .show()?;
    Ok(())
}

fn is_timestamp_over(timestamp: i64) -> Result<bool> {
    let time = utils::get_local_time(timestamp)?;
    let current_time = Local::now();
    Ok(current_time >= time)
}

pub fn add_epg(state: State<'_, Mutex<AppState>>, app: AppHandle, epg: EPGNotify) -> Result<()> {
    let mut state = state.lock().unwrap();
    if state.thread_handle.is_some() {
        state.notify_stop.store(true, Relaxed);
        let _ = state
            .thread_handle
            .take()
            .context("no thread in option")?
            .join();
    }
    let stop = state.notify_stop.clone();
    sql::clean_epgs()?;
    sql::add_epg(epg)?;
    let list = sql::get_epgs()?;
    state
        .thread_handle
        .replace(thread::spawn(|| poll(list, stop, app)));
    Ok(())
}

pub fn remove_epg(state: State<'_, Mutex<AppState>>, app: AppHandle, epg_id: String) -> Result<()> {
    let mut state = state.lock().unwrap();
    if state.thread_handle.is_some() {
        state.notify_stop.store(true, Relaxed);
        let _ = state
            .thread_handle
            .take()
            .context("no thread in option")?
            .join();
    }
    let stop = state.notify_stop.clone();
    sql::clean_epgs()?;
    sql::remove_epg(epg_id)?;
    let list = sql::get_epgs()?;
    if list.len() == 0 {
        return Ok(());
    }
    state
        .thread_handle
        .replace(thread::spawn(|| poll(list, stop, app)));
    Ok(())
}
