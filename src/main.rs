use serde::{Deserialize, Serialize};
use skim::prelude::*;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Cursor};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::{SystemTime, UNIX_EPOCH};

const APP_DIR_NAME: &str = "s";
const HISTORY_FILE_NAME: &str = "history.json";

#[derive(Debug, Serialize, Deserialize)]
struct HistoryEntry {
    target: String,
    last_used_at: i64,
    use_count: u64,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.as_slice() {
        [] => run_interactive(),
        [target] if target == "-h" || target == "--help" => {
            print_help();
            Ok(())
        }
        [target] => {
            let target = target.trim();
            if target.is_empty() {
                return Err("target must not be empty".to_string());
            }

            let history_path = history_path()?;
            let mut history = load_history(&history_path)?;
            record_target(&mut history, target)?;
            save_history(&history_path, &history)?;
            exec_ssh(target)
        }
        _ => Err("usage: s [target]".to_string()),
    }
}

fn run_interactive() -> Result<(), String> {
    let history_path = history_path()?;
    let mut history = load_history(&history_path)?;
    sort_history(&mut history);

    if history.is_empty() {
        println!("No SSH history yet. Use: s <target>");
        return Ok(());
    }

    let selected = select_target(&history)?;
    exec_ssh(&selected)
}

fn print_help() {
    println!("Usage:");
    println!("  s          Select a recent SSH target");
    println!("  s <target> Record target and run ssh <target>");
}

fn select_target(history: &[HistoryEntry]) -> Result<String, String> {
    let options = SkimOptionsBuilder::default()
        .height(Some("50%"))
        .prompt(Some("s> "))
        .multi(false)
        .reverse(true)
        .build()
        .map_err(|err| format!("failed to configure selector: {err}"))?;

    let input = Cursor::new(history_targets(history));
    let items = SkimItemReader::default().of_bufread(input);
    let output = Skim::run_with(&options, Some(items))
        .ok_or_else(|| "failed to start interactive selector".to_string())?;

    if output.is_abort {
        process::exit(1);
    }

    let selected = output
        .selected_items
        .first()
        .map(|item| item.output().to_string())
        .unwrap_or_default();

    if selected.is_empty() {
        process::exit(1);
    }

    Ok(selected)
}

fn record_target(history: &mut Vec<HistoryEntry>, target: &str) -> Result<(), String> {
    let now = unix_timestamp_now()?;

    if let Some(entry) = history.iter_mut().find(|entry| entry.target == target) {
        entry.last_used_at = now;
        entry.use_count = entry
            .use_count
            .checked_add(1)
            .ok_or_else(|| format!("use_count overflow for target: {target}"))?;
        return Ok(());
    }

    history.push(HistoryEntry {
        target: target.to_string(),
        last_used_at: now,
        use_count: 1,
    });

    Ok(())
}

fn sort_history(history: &mut [HistoryEntry]) {
    history.sort_by(|a, b| {
        b.last_used_at
            .cmp(&a.last_used_at)
            .then_with(|| b.use_count.cmp(&a.use_count))
            .then_with(|| a.target.cmp(&b.target))
    });
}

fn history_targets(history: &[HistoryEntry]) -> String {
    let mut buffer = String::new();
    for entry in history {
        buffer.push_str(&entry.target);
        buffer.push('\n');
    }
    buffer
}

fn exec_ssh(target: &str) -> Result<(), String> {
    let err = Command::new("ssh").arg(target).exec();
    match err.kind() {
        io::ErrorKind::NotFound => Err("ssh is required but was not found in PATH".to_string()),
        _ => Err(format!("failed to exec ssh: {err}")),
    }
}

fn load_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map_err(|err| format!("failed to parse history file {}: {err}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(format!(
            "failed to read history file {}: {err}",
            path.display()
        )),
    }
}

fn save_history(path: &Path, history: &[HistoryEntry]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("history path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|err| {
        format!(
            "failed to create config directory {}: {err}",
            parent.display()
        )
    })?;

    let tmp_path = temp_history_path(path);
    let payload = serde_json::to_vec_pretty(history)
        .map_err(|err| format!("failed to serialize history: {err}"))?;

    fs::write(&tmp_path, payload)
        .map_err(|err| format!("failed to write history file {}: {err}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).map_err(|err| {
        format!(
            "failed to move history file {} to {}: {err}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

fn temp_history_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(HISTORY_FILE_NAME);
    path.with_file_name(format!("{file_name}.tmp"))
}

fn history_path() -> Result<PathBuf, String> {
    history_path_from_env(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn history_path_from_env(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, String> {
    if let Some(xdg_config_home) = xdg_config_home {
        if !xdg_config_home.is_empty() {
            return Ok(PathBuf::from(xdg_config_home)
                .join(APP_DIR_NAME)
                .join(HISTORY_FILE_NAME));
        }
    }

    let home = home.ok_or_else(|| {
        "could not determine config directory: neither XDG_CONFIG_HOME nor HOME is set".to_string()
    })?;

    Ok(PathBuf::from(home)
        .join(".config")
        .join(APP_DIR_NAME)
        .join(HISTORY_FILE_NAME))
}

fn unix_timestamp_now() -> Result<i64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("system clock is before UNIX_EPOCH: {err}"))?;

    i64::try_from(duration.as_secs()).map_err(|_| "unix timestamp overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_target_adds_new_entry() {
        let mut history = Vec::new();

        record_target(&mut history, "prod").expect("record should succeed");

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].target, "prod");
        assert_eq!(history[0].use_count, 1);
        assert!(history[0].last_used_at > 0);
    }

    #[test]
    fn record_target_updates_existing_entry() {
        let mut history = vec![HistoryEntry {
            target: "prod".to_string(),
            last_used_at: 1,
            use_count: 7,
        }];

        record_target(&mut history, "prod").expect("record should succeed");

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].target, "prod");
        assert_eq!(history[0].use_count, 8);
        assert!(history[0].last_used_at >= 1);
    }

    #[test]
    fn sort_history_orders_by_recency_then_count_then_target() {
        let mut history = vec![
            HistoryEntry {
                target: "b".to_string(),
                last_used_at: 10,
                use_count: 1,
            },
            HistoryEntry {
                target: "a".to_string(),
                last_used_at: 10,
                use_count: 2,
            },
            HistoryEntry {
                target: "c".to_string(),
                last_used_at: 20,
                use_count: 1,
            },
        ];

        sort_history(&mut history);

        let targets: Vec<&str> = history.iter().map(|entry| entry.target.as_str()).collect();
        assert_eq!(targets, vec!["c", "a", "b"]);
    }

    #[test]
    fn history_targets_formats_one_target_per_line() {
        let history = vec![
            HistoryEntry {
                target: "prod".to_string(),
                last_used_at: 1,
                use_count: 1,
            },
            HistoryEntry {
                target: "root@1.2.3.4".to_string(),
                last_used_at: 2,
                use_count: 3,
            },
        ];

        assert_eq!(history_targets(&history), "prod\nroot@1.2.3.4\n");
    }

    #[test]
    fn history_path_prefers_xdg_config_home() {
        let path = history_path_from_env(Some("/tmp/xdg".into()), Some("/tmp/home".into()))
            .expect("path should resolve");

        assert_eq!(path, PathBuf::from("/tmp/xdg/s/history.json"));
    }

    #[test]
    fn history_path_falls_back_to_home_config() {
        let path =
            history_path_from_env(None, Some("/tmp/home".into())).expect("path should resolve");

        assert_eq!(path, PathBuf::from("/tmp/home/.config/s/history.json"));
    }

    #[test]
    fn load_history_returns_empty_when_file_is_missing() {
        let path = unique_test_path("missing-history.json");
        let history = load_history(&path).expect("load should succeed");
        assert!(history.is_empty());
    }

    #[test]
    fn save_and_load_history_round_trip() {
        let path = unique_test_path("round-trip-history.json");
        let history = vec![HistoryEntry {
            target: "prod".to_string(),
            last_used_at: 42,
            use_count: 9,
        }];

        save_history(&path, &history).expect("save should succeed");
        let loaded = load_history(&path).expect("load should succeed");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].target, "prod");
        assert_eq!(loaded[0].last_used_at, 42);
        assert_eq!(loaded[0].use_count, 9);

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn unique_test_path(file_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();

        env::temp_dir()
            .join(format!("ssh-oxide-test-{}-{nanos}", process::id()))
            .join(file_name)
    }
}
