use crate::types::ChannelHttpHeaders;
use crate::{log, sql};
use crate::{media_type, settings::get_settings, types::Channel};
use anyhow::{bail, Context, Result};
use chrono::Local;
use directories::UserDirs;
use std::sync::LazyLock;
use std::{
    env::{consts::OS, current_exe},
    path::Path,
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use which::which;

const ARG_SAVE_POSITION_ON_QUIT: &str = "--save-position-on-quit";
const ARG_CACHE: &str = "--cache=";
const ARG_NO: &str = "no";
const ARG_RECORD: &str = "--stream-record=";
const ARG_TITLE: &str = "--title=";
const ARG_MSG_LEVEL: &str = "--msg-level=all=error";
const ARG_YTDLP_PATH: &str = "--script-opts=ytdl_hook-ytdl_path=";
const ARG_VOLUME: &str = "--volume=";
const ARG_HTTP_HEADERS: &str = "--http-header-fields=";
const ARG_USER_AGENT: &str = "--user-agent=";
const ARG_IGNORE_SSL: &str = "--ytdl-raw-options=no-check-certificates=True";
const MPV_BIN_NAME: &str = "mpv";
const YTDLP_BIN_NAME: &str = "yt-dlp";
const HTTP_ORIGIN: &str = "origin:";
const HTTP_REFERRER: &str = "referer:";
const MACOS_POTENTIAL_PATHS: [&str; 3] = [
    "/opt/local/bin",    // MacPorts
    "/opt/homebrew/bin", // Homebrew on AARCH64 Mac
    "/usr/local/bin",    // Homebrew on AMD64 Mac
];

static MPV_PATH: LazyLock<String> = LazyLock::new(|| get_mpv_path());
static YTDLP_PATH: LazyLock<String> = LazyLock::new(|| find_macos_bin(YTDLP_BIN_NAME.to_string()));

pub async fn play(channel: Channel, record: bool) -> Result<()> {
    println!("{} playing", channel.url.as_ref().unwrap());
    let args = get_play_args(channel, record)?;
    let mut cmd = Command::new(MPV_PATH.clone())
        .args(args)
        .stdout(Stdio::piped())
        .spawn()?;

    let status = cmd.wait().await?;
    if !status.success() {
        let stdout = cmd.stdout.take();
        if let Some(stdout) = stdout {
            let mut error: String = "".to_string();
            let mut lines = BufReader::new(stdout).lines();
            let mut first = true;
            while let Some(line) = lines.next_line().await? {
                error += &line;
                if !first {
                    error += "\n"
                } else {
                    first = false;
                }
            }
            if error != "" {
                bail!(error);
            } else {
                bail!("Mpv encountered an unknown error");
            }
        }
    }
    Ok(())
}

fn get_mpv_path() -> String {
    if OS == "linux" || which("mpv").is_ok() {
        return MPV_BIN_NAME.to_string();
    } else if OS == "macos" {
        return find_macos_bin(MPV_BIN_NAME.to_string());
    }
    return get_mpv_path_win();
}

fn get_mpv_path_win() -> String {
    let mut path = current_exe().unwrap();
    path.pop();
    path.push("deps");
    path.push("mpv.exe");
    return path.to_string_lossy().to_string();
}

fn find_macos_bin(bin: String) -> String {
    return MACOS_POTENTIAL_PATHS
        .iter()
        .map(|path| {
            let mut path = Path::new(path).to_path_buf();
            path.push(&bin);
            return path;
        })
        .find(|path| path.exists())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            log::log(format!("Could not find {} on MacOS host", bin));
            return bin;
        });
}

fn get_play_args(channel: Channel, record: bool) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let settings = get_settings()?;
    let headers = sql::get_channel_headers_by_id(channel.id.context("no channel id?")?)?;
    args.push(channel.url.context("no url")?);
    if channel.media_type != media_type::LIVESTREAM {
        args.push(ARG_SAVE_POSITION_ON_QUIT.to_string());
    }
    if settings.use_stream_caching == Some(false) {
        let stream_caching_arg = format!("{ARG_CACHE}{ARG_NO}",);
        args.push(stream_caching_arg);
    }
    if record {
        let record_path = match settings.recording_path {
            Some(path) => get_path(path),
            None => get_default_record_path()?,
        };
        args.push(format!("{ARG_RECORD}{record_path}"));
    }
    if OS == "macos" && *MPV_PATH != MPV_BIN_NAME {
        args.push(format!("{}{}", ARG_YTDLP_PATH, *YTDLP_PATH));
    }
    args.push(format!("{}{}", ARG_TITLE, channel.name));
    args.push(ARG_MSG_LEVEL.to_string());
    if let Some(volume) = settings.volume {
        args.push(format!("{ARG_VOLUME}{volume}"));
    }
    set_headers(headers, &mut args);
    if let Some(mpv_params) = settings.mpv_params {
        #[cfg(not(target_os = "windows"))]
        let mut params = shell_words::split(&mpv_params)?;
        #[cfg(target_os = "windows")]
        let mut params = winsplit::split(&mpv_params);
        args.append(&mut params);
    }
    Ok(args)
}

fn set_headers(headers: Option<ChannelHttpHeaders>, args: &mut Vec<String>) {
    if headers.is_none() {
        return;
    }
    let headers = headers.unwrap();
    let mut headers_vec: Vec<String> = Vec::with_capacity(2);
    if let Some(origin) = headers.http_origin {
        headers_vec.push(format!("{HTTP_ORIGIN}{origin}"));
    }
    if let Some(referrer) = headers.referrer {
        headers_vec.push(format!("{HTTP_REFERRER}{referrer}"));
    }
    if let Some(user_agent) = headers.user_agent {
        args.push(format!("{ARG_USER_AGENT}{user_agent}"));
    }
    if headers.ignore_ssl {
        args.push(ARG_IGNORE_SSL.to_string());
    }
    let headers = headers_vec.join(",");
    args.push(format!("{ARG_HTTP_HEADERS}{headers}"));
}

fn get_path(path_str: String) -> String {
    let path = Path::new(&path_str);
    let path = path.join(get_file_name());
    return path.to_string_lossy().to_string(); // Check if it causes problems for some OS languages?
}

fn get_file_name() -> String {
    let current_time = Local::now();
    let formatted_time = current_time.format("%Y-%m-%d-%H-%M-%S").to_string();
    format!("{formatted_time}.mp4")
}

fn get_default_record_path() -> Result<String> {
    let user_dirs = UserDirs::new().context("Failed to get user dirs")?;
    let mut path = user_dirs.video_dir().context("No videos dir")?.to_owned();
    path.push("open-tv");
    std::fs::create_dir_all(&path)?;
    Ok(path.to_string_lossy().to_string())
}
