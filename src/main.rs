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

#[derive(Debug, PartialEq, Eq)]
enum Action {
    InteractiveConnect,
    Connect {
        target: String,
    },
    InteractiveAddKey(AddKeyOptions),
    AddKey {
        target: String,
        options: AddKeyOptions,
    },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AddKeyOptions {
    port: Option<u16>,
    key_path: Option<PathBuf>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let action = parse_action(&args)?;

    match action {
        Action::InteractiveConnect => run_interactive_connect(),
        Action::Connect { target } => connect_to_target(&target),
        Action::InteractiveAddKey(options) => run_interactive_add_key(options),
        Action::AddKey { target, options } => add_key_to_target(&target, &options),
        Action::Help => {
            print_help();
            Ok(())
        }
    }
}

fn parse_action(args: &[String]) -> Result<Action, String> {
    match args {
        [] => Ok(Action::InteractiveConnect),
        [flag] if flag == "-h" || flag == "--help" => Ok(Action::Help),
        [command, rest @ ..] if command == "add-key" => parse_add_key_action(rest),
        [target] => {
            let target = target.trim();
            if target.is_empty() {
                return Err(
                    "usage: s [target] | s add-key [--key <path>] [-p <port>] [target]".to_string(),
                );
            }
            Ok(Action::Connect {
                target: target.to_string(),
            })
        }
        _ => Err("usage: s [target] | s add-key [--key <path>] [-p <port>] [target]".to_string()),
    }
}

fn run_interactive_connect() -> Result<(), String> {
    let history_path = history_path()?;
    let mut history = load_history(&history_path)?;
    sort_history(&mut history);

    if history.is_empty() {
        println!("No SSH history yet. Use: s <target>");
        return Ok(());
    }

    let selected = select_target(&history, "s> ")?;
    exec_ssh(&selected, None)
}

fn run_interactive_add_key(options: AddKeyOptions) -> Result<(), String> {
    let history_path = history_path()?;
    let mut history = load_history(&history_path)?;
    sort_history(&mut history);

    if history.is_empty() {
        println!("No SSH history yet. Use: s add-key <target>");
        return Ok(());
    }

    let selected = select_target(&history, "add-key> ")?;
    add_key_to_target(&selected, &options)
}

fn connect_to_target(target: &str) -> Result<(), String> {
    record_history_for_target(target)?;
    exec_ssh(target, None)
}

fn add_key_to_target(target: &str, options: &AddKeyOptions) -> Result<(), String> {
    let home = home_dir()?;
    let resolved_key = resolve_public_key_path(&home, options.key_path.as_deref())?;
    let public_key = fs::read_to_string(&resolved_key).map_err(|err| {
        format!(
            "failed to read public key {}: {err}",
            resolved_key.display()
        )
    })?;
    let public_key = public_key.trim();

    if public_key.is_empty() {
        return Err(format!(
            "public key file is empty: {}",
            resolved_key.display()
        ));
    }

    println!("Using public key: {}", resolved_key.display());
    record_history_for_target(target)?;

    let ssh_copy_id_error = exec_ssh_copy_id(target, options.port, &resolved_key);
    if matches!(ssh_copy_id_error, Err(ref err) if err.kind() == io::ErrorKind::NotFound) {
        let command = build_authorized_keys_command(public_key);
        exec_manual_add_key(target, options.port, &command)
    } else {
        match ssh_copy_id_error {
            Ok(()) => Ok(()),
            Err(err) => Err(format!("failed to exec ssh-copy-id: {err}")),
        }
    }
}

fn print_help() {
    println!("Usage:");
    println!("  s");
    println!("  s <target>");
    println!("  s add-key [--key <path>] [-p <port>] [target]");
}

fn select_target(history: &[HistoryEntry], prompt: &str) -> Result<String, String> {
    let options = SkimOptionsBuilder::default()
        .height(Some("50%"))
        .prompt(Some(prompt))
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

fn resolve_public_key_path(home: &Path, explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        let expanded = expand_tilde(explicit, home);
        if expanded.is_file() {
            return Ok(expanded);
        }
        return Err(format!(
            "public key file does not exist: {}",
            expanded.display()
        ));
    }

    let candidates = [
        ".ssh/id_ed25519.pub",
        ".ssh/id_ecdsa.pub",
        ".ssh/id_rsa.pub",
        ".ssh/id_dsa.pub",
    ];

    for candidate in candidates {
        let path = home.join(candidate);
        if path.is_file() {
            return Ok(path);
        }
    }

    Err(format!(
        "no public key found; tried {}",
        candidates
            .iter()
            .map(|candidate| home.join(candidate).display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn build_authorized_keys_command(public_key: &str) -> String {
    let quoted = shell_single_quote(public_key);
    format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && grep -qxF {quoted} ~/.ssh/authorized_keys || printf '%s\\n' {quoted} >> ~/.ssh/authorized_keys"
    )
}

fn exec_ssh(target: &str, port: Option<u16>) -> Result<(), String> {
    let mut command = Command::new("ssh");
    if let Some(port) = port {
        command.arg("-p").arg(port.to_string());
    }
    let err = command.arg(target).exec();
    match err.kind() {
        io::ErrorKind::NotFound => Err("ssh is required but was not found in PATH".to_string()),
        _ => Err(format!("failed to exec ssh: {err}")),
    }
}

fn exec_ssh_copy_id(target: &str, port: Option<u16>, key_path: &Path) -> io::Result<()> {
    let mut command = Command::new("ssh-copy-id");
    command.arg("-i").arg(key_path);
    if let Some(port) = port {
        command.arg("-p").arg(port.to_string());
    }
    let err = command.arg(target).exec();
    Err(err)
}

fn exec_manual_add_key(
    target: &str,
    port: Option<u16>,
    remote_command: &str,
) -> Result<(), String> {
    let mut command = Command::new("ssh");
    if let Some(port) = port {
        command.arg("-p").arg(port.to_string());
    }
    let err = command
        .arg(target)
        .arg("sh")
        .arg("-c")
        .arg(remote_command)
        .exec();
    match err.kind() {
        io::ErrorKind::NotFound => Err("ssh is required but was not found in PATH".to_string()),
        _ => Err(format!("failed to exec ssh: {err}")),
    }
}

fn record_history_for_target(target: &str) -> Result<(), String> {
    let history_path = history_path()?;
    let mut history = load_history(&history_path)?;
    record_target(&mut history, target)?;
    save_history(&history_path, &history)
}

fn parse_add_key_action(args: &[String]) -> Result<Action, String> {
    let mut options = AddKeyOptions::default();
    let mut target: Option<String> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-p" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    "usage: s add-key [--key <path>] [-p <port>] [target]".to_string()
                })?;
                options.port = Some(parse_port(value)?);
            }
            "--key" => {
                index += 1;
                let value = args.get(index).ok_or_else(|| {
                    "usage: s add-key [--key <path>] [-p <port>] [target]".to_string()
                })?;
                options.key_path = Some(PathBuf::from(value));
            }
            value if value.starts_with('-') => {
                return Err(format!(
                    "unknown add-key option: {value}\nusage: s add-key [--key <path>] [-p <port>] [target]"
                ))
            }
            value => {
                if target.is_some() {
                    return Err(
                        "usage: s add-key [--key <path>] [-p <port>] [target]".to_string()
                    );
                }
                target = Some(value.to_string());
            }
        }
        index += 1;
    }

    Ok(match target {
        Some(target) => Action::AddKey { target, options },
        None => Action::InteractiveAddKey(options),
    })
}

fn parse_port(value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("invalid port: {value}"))
}

fn home_dir() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .ok_or_else(|| "could not determine home directory: HOME is not set".to_string())?;
    Ok(PathBuf::from(home))
}

fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };

    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(stripped) = raw.strip_prefix("~/") {
        return home.join(stripped);
    }

    path.to_path_buf()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
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
    fn parse_action_defaults_to_interactive_connect() {
        let action = parse_action(&[]).expect("parse should succeed");
        assert_eq!(action, Action::InteractiveConnect);
    }

    #[test]
    fn parse_action_supports_help() {
        let args = vec!["--help".to_string()];
        let action = parse_action(&args).expect("parse should succeed");
        assert_eq!(action, Action::Help);
    }

    #[test]
    fn parse_action_supports_connect_target() {
        let args = vec!["prod".to_string()];
        let action = parse_action(&args).expect("parse should succeed");
        assert_eq!(
            action,
            Action::Connect {
                target: "prod".to_string()
            }
        );
    }

    #[test]
    fn parse_action_supports_interactive_add_key() {
        let args = vec!["add-key".to_string()];
        let action = parse_action(&args).expect("parse should succeed");
        assert_eq!(action, Action::InteractiveAddKey(AddKeyOptions::default()));
    }

    #[test]
    fn parse_action_supports_add_key_target_and_options() {
        let args = vec![
            "add-key".to_string(),
            "-p".to_string(),
            "2222".to_string(),
            "--key".to_string(),
            "~/.ssh/custom.pub".to_string(),
            "root@1.2.3.4".to_string(),
        ];
        let action = parse_action(&args).expect("parse should succeed");
        assert_eq!(
            action,
            Action::AddKey {
                target: "root@1.2.3.4".to_string(),
                options: AddKeyOptions {
                    port: Some(2222),
                    key_path: Some(PathBuf::from("~/.ssh/custom.pub")),
                },
            }
        );
    }

    #[test]
    fn parse_action_rejects_extra_positional_args() {
        let args = vec![
            "add-key".to_string(),
            "prod".to_string(),
            "extra".to_string(),
        ];
        let err = parse_action(&args).expect_err("parse should fail");
        assert!(err.contains("usage"));
    }

    #[test]
    fn resolve_public_key_prefers_explicit_path() {
        let home = unique_test_dir("explicit-key-home");
        let explicit = home.join("custom.pub");
        fs::create_dir_all(&home).expect("dir should exist");
        fs::write(&explicit, "ssh-ed25519 AAAA test@example\n").expect("write should succeed");

        let resolved =
            resolve_public_key_path(&home, Some(&explicit)).expect("resolution should succeed");

        assert_eq!(resolved, explicit);
        cleanup_path(&home);
    }

    #[test]
    fn resolve_public_key_prefers_default_ed25519() {
        let home = unique_test_dir("default-key-home");
        let ssh_dir = home.join(".ssh");
        let default_key = ssh_dir.join("id_ed25519.pub");
        let fallback_key = ssh_dir.join("id_rsa.pub");
        fs::create_dir_all(&ssh_dir).expect("dir should exist");
        fs::write(&default_key, "ssh-ed25519 AAAA primary\n").expect("write should succeed");
        fs::write(&fallback_key, "ssh-rsa AAAA fallback\n").expect("write should succeed");

        let resolved = resolve_public_key_path(&home, None).expect("resolution should succeed");

        assert_eq!(resolved, default_key);
        cleanup_path(&home);
    }

    #[test]
    fn resolve_public_key_falls_back_to_known_candidates() {
        let home = unique_test_dir("fallback-key-home");
        let ssh_dir = home.join(".ssh");
        let fallback_key = ssh_dir.join("id_rsa.pub");
        fs::create_dir_all(&ssh_dir).expect("dir should exist");
        fs::write(&fallback_key, "ssh-rsa AAAA fallback\n").expect("write should succeed");

        let resolved = resolve_public_key_path(&home, None).expect("resolution should succeed");

        assert_eq!(resolved, fallback_key);
        cleanup_path(&home);
    }

    #[test]
    fn build_authorized_keys_command_escapes_single_quotes() {
        let command = build_authorized_keys_command("ssh-ed25519 AAAA comment'o");
        assert!(command.contains("grep -qxF"));
        assert!(command.contains("'\"'\"'"));
    }

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

        cleanup_file_and_parent(&path);
    }

    fn unique_test_path(file_name: &str) -> PathBuf {
        unique_test_dir("io-test").join(file_name)
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();

        env::temp_dir().join(format!("ssh-oxide-{label}-{}-{nanos}", process::id()))
    }

    fn cleanup_file_and_parent(path: &Path) {
        let _ = fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn cleanup_path(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
}
