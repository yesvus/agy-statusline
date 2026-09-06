use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GRAY: &str = "\x1b[90m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuotaItem {
    #[serde(default)]
    remaining_fraction: Option<f64>,
    #[serde(default)]
    reset_time: Option<String>,
    #[serde(default)]
    reset_in_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuotaCache {
    saved_at: f64,
    quota: HashMap<String, QuotaItem>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    display_name: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    current_dir: Option<String>,
    project_dir: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContextWindowInfo {
    total_input_tokens: Option<f64>,
    total_tokens: Option<f64>,
    context_window_size: Option<f64>,
    used_percentage: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct InputData {
    model: Option<ModelInfo>,
    workspace: Option<WorkspaceInfo>,
    cwd: Option<String>,
    context_window: Option<ContextWindowInfo>,
    quota: Option<HashMap<String, QuotaItem>>,
}

fn color_for(pct: f64) -> &'static str {
    if pct < 20.0 {
        RED
    } else if pct < 50.0 {
        YELLOW
    } else {
        GREEN
    }
}

fn fmt_reset(secs: i64) -> String {
    let secs = secs.max(0);
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        if h > 0 {
            format!("{d}d{h}h")
        } else {
            format!("{d}d")
        }
    } else if h > 0 {
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else {
        format!("{m}m")
    }
}

fn fmt_tokens(val: f64) -> String {
    let num = val.round() as i64;
    if num >= 1_000_000 {
        format!("{}M", num / 1_000_000)
    } else if num >= 1_000 {
        format!("{}k", num / 1_000)
    } else {
        format!("{num}")
    }
}

fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn get_git_branch(mut dir: &Path) -> Option<String> {
    loop {
        let git_entry = dir.join(".git");
        if git_entry.is_dir() {
            let head_file = git_entry.join("HEAD");
            return parse_git_head(&head_file);
        } else if git_entry.is_file() {
            if let Ok(content) = std::fs::read_to_string(&git_entry) {
                if let Some(line) = content.lines().next() {
                    if let Some(rel_or_abs) = line.strip_prefix("gitdir:") {
                        let gitdir = rel_or_abs.trim();
                        let target: PathBuf = if Path::new(gitdir).is_absolute() {
                            gitdir.into()
                        } else {
                            dir.join(gitdir)
                        };
                        return parse_git_head(&target.join("HEAD"));
                    }
                }
            }
            return None;
        }

        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    None
}

fn parse_git_head(head_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(head_path).ok()?;
    let line = content.lines().next()?.trim();
    if let Some(branch) = line.strip_prefix("ref: refs/heads/") {
        let mut b = branch.to_string();
        if b.chars().count() > 20 {
            let truncated: String = b.chars().take(19).collect();
            b = format!("{truncated}…");
        }
        Some(b)
    } else if line.len() >= 7 {
        let short_sha: String = line.chars().take(7).collect();
        Some(short_sha)
    } else {
        None
    }
}

fn sync_and_get_quota(incoming_quota: Option<HashMap<String, QuotaItem>>) -> HashMap<String, QuotaItem> {
    let uid = unsafe { libc::getuid() };
    let cache_path = format!("/dev/shm/agy_quota.{uid}.json");
    let lock_path = format!("/dev/shm/agy_quota.{uid}.lock");

    let lock_file = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(_) => return incoming_quota.unwrap_or_default(),
    };

    unsafe {
        libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX);
    }

    let mut cached_quota: HashMap<String, QuotaItem> = match std::fs::read_to_string(&cache_path) {
        Ok(content) => match serde_json::from_str::<QuotaCache>(&content) {
            Ok(c) => c.quota,
            Err(_) => HashMap::new(),
        },
        Err(_) => HashMap::new(),
    };

    let now_utc = Utc::now();

    if let Some(inc) = incoming_quota {
        for (k, inc_val) in inc {
            if inc_val.remaining_fraction.is_none() {
                continue;
            }
            if let Some(cur_val) = cached_quota.get_mut(&k) {
                let inc_reset = inc_val.reset_time.as_deref().and_then(parse_iso);
                let cur_reset = cur_val.reset_time.as_deref().and_then(parse_iso);

                if let (Some(ir), Some(cr)) = (inc_reset, cur_reset) {
                    if ir > cr {
                        *cur_val = inc_val;
                        continue;
                    }
                } else if inc_val.reset_time != cur_val.reset_time && inc_val.reset_time.is_some() {
                    *cur_val = inc_val;
                    continue;
                }

                // Same window: lowest remaining fraction wins
                let inc_frac = inc_val.remaining_fraction.unwrap_or(1.0);
                let cur_frac = cur_val.remaining_fraction.unwrap_or(1.0);
                cur_val.remaining_fraction = Some(inc_frac.min(cur_frac));
                if inc_val.reset_time.is_some() {
                    cur_val.reset_time = inc_val.reset_time;
                }
                if inc_val.reset_in_seconds.is_some() {
                    cur_val.reset_in_seconds = inc_val.reset_in_seconds;
                }
            } else {
                cached_quota.insert(k, inc_val);
            }
        }
    }

    let cache_obj = QuotaCache {
        saved_at: now_utc.timestamp() as f64,
        quota: cached_quota.clone(),
    };
    let pid = std::process::id();
    let tmp_path = format!("{cache_path}.tmp.{pid}");
    if let Ok(json_str) = serde_json::to_string(&cache_obj) {
        if std::fs::write(&tmp_path, json_str).is_ok() {
            let _ = std::fs::rename(&tmp_path, &cache_path);
        }
    }

    unsafe {
        libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
    }

    for (_, val) in cached_quota.iter_mut() {
        if let Some(ref rt) = val.reset_time {
            if let Some(r_dt) = parse_iso(rt) {
                let diff = (r_dt - now_utc).num_seconds().max(0);
                val.reset_in_seconds = Some(diff);
            }
        }
    }

    cached_quota
}

fn fmt_quota_item(q: Option<&QuotaItem>) -> String {
    let q = match q {
        Some(q) => q,
        None => return String::new(),
    };
    let frac = match q.remaining_fraction {
        Some(f) => f,
        None => return String::new(),
    };
    let pct = (frac * 100.0).round() as i64;
    let color = color_for(pct as f64);
    let reset_str = q.reset_in_seconds.map(fmt_reset).unwrap_or_default();
    if !reset_str.is_empty() {
        format!("{color}{pct}%({reset_str}){RESET}")
    } else {
        format!("{color}{pct}%{RESET}")
    }
}

fn main() {
    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() || raw.trim().is_empty() {
        return;
    }

    let data: InputData = match serde_json::from_str(&raw) {
        Ok(d) => d,
        Err(_) => return,
    };

    // 1. Model
    let model = data
        .model
        .as_ref()
        .and_then(|m| m.display_name.clone().or_else(|| m.id.clone()))
        .unwrap_or_else(|| "unknown".to_string());

    // 2. Directory / Project (Project specific)
    let ws = data.workspace.as_ref();
    let cwd = ws
        .and_then(|w| w.project_dir.as_deref())
        .or_else(|| ws.and_then(|w| w.current_dir.as_deref()))
        .or_else(|| ws.and_then(|w| w.cwd.as_deref()))
        .or(data.cwd.as_deref())
        .unwrap_or("");

    let home = std::env::var("HOME").unwrap_or_default();
    let dir_short = if cwd == home {
        "~".to_string()
    } else if !cwd.is_empty() {
        Path::new(cwd.trim_end_matches('/'))
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Git branch (in-process resolution)
    let branch = if !cwd.is_empty() {
        get_git_branch(Path::new(cwd))
    } else {
        None
    };

    // 3. Context window (Project specific)
    let cw = data.context_window.as_ref();
    let pct = cw.and_then(|c| c.used_percentage).or_else(|| {
        if let Some(c) = cw {
            let tokens = c.total_input_tokens.or(c.total_tokens)?;
            let max = c.context_window_size?;
            if max > 0.0 {
                return Some((tokens * 100.0) / max);
            }
        }
        None
    });

    let mut ctx_str = String::new();
    if let Some(p) = pct {
        let pct_val = p.round() as i64;
        let color = if pct_val >= 80 {
            RED
        } else if pct_val >= 50 {
            YELLOW
        } else {
            GREEN
        };
        let mut ctx_size = String::new();
        if let Some(c) = cw {
            let cur = c.total_input_tokens.or(c.total_tokens);
            let max = c.context_window_size;
            if let (Some(cur_val), Some(max_val)) = (cur, max) {
                ctx_size = format!("{}/{}", fmt_tokens(cur_val), fmt_tokens(max_val));
            } else if let Some(cur_val) = cur {
                ctx_size = fmt_tokens(cur_val);
            }
        }
        if !ctx_size.is_empty() {
            ctx_str = format!("{color}ctx {pct_val}%{RESET} {DIM}({ctx_size}){RESET}");
        } else {
            ctx_str = format!("{color}ctx {pct_val}%{RESET}");
        }
    }

    // 4. Shared Quota (Global across projects)
    let quota = sync_and_get_quota(data.quota);

    let find_quota = |test: fn(&str) -> bool| -> Option<&QuotaItem> {
        quota.iter().find(|(k, _)| test(&k.to_lowercase())).map(|(_, v)| v)
    };

    let g5h_item = fmt_quota_item(find_quota(|s| {
        (s.contains("gemini") && (s.contains("5h") || s.contains("5hour")))
            || s.starts_with("5h")
            || s.starts_with("5hour")
    }));

    let p3_5h_item = fmt_quota_item(find_quota(|s| {
        s.contains("3p") && (s.contains("5h") || s.contains("5hour"))
    }));

    let gw_item = fmt_quota_item(find_quota(|s| {
        (s.contains("gemini") && s.contains("weekly")) || s.starts_with("weekly")
    }));

    let p3w_item = fmt_quota_item(find_quota(|s| {
        s.contains("3p") && s.contains("weekly")
    }));

    let mut g5h_group = String::new();
    if !g5h_item.is_empty() && !p3_5h_item.is_empty() {
        g5h_group = format!("5H {g5h_item} {DIM}|{RESET} 3P {p3_5h_item}");
    } else if !g5h_item.is_empty() {
        g5h_group = format!("5H {g5h_item}");
    } else if !p3_5h_item.is_empty() {
        g5h_group = format!("5H 3P {p3_5h_item}");
    }

    let mut gw_group = String::new();
    if !gw_item.is_empty() && !p3w_item.is_empty() {
        gw_group = format!("W {gw_item} {DIM}|{RESET} 3P {p3w_item}");
    } else if !gw_item.is_empty() {
        gw_group = format!("W {gw_item}");
    } else if !p3w_item.is_empty() {
        gw_group = format!("W 3P {p3w_item}");
    }

    let sep = format!(" {DIM}·{RESET} ");
    let mut row1_parts = vec![format!("{CYAN}{BOLD}{model}{RESET}")];
    if !dir_short.is_empty() {
        if let Some(br) = branch {
            row1_parts.push(format!("{GRAY}{dir_short}{RESET} {DIM}({br}){RESET}"));
        } else {
            row1_parts.push(format!("{GRAY}{dir_short}{RESET}"));
        }
    }
    if !ctx_str.is_empty() {
        row1_parts.push(ctx_str);
    }

    let mut row2_parts = Vec::new();
    if !g5h_group.is_empty() {
        row2_parts.push(g5h_group);
    }
    if !gw_group.is_empty() {
        row2_parts.push(gw_group);
    }

    let row1 = row1_parts.join(&sep);
    let row2 = row2_parts.join(&sep);

    if !row2.is_empty() {
        print!("{row1}\n{row2}");
    } else {
        print!("{row1}");
    }
    let _ = io::stdout().flush();
}
