# s

`s` is a tiny Rust CLI that wraps the system OpenSSH client and keeps a local history of recently used SSH targets.

When you run `s` with no arguments, it loads your saved targets, opens an embedded fuzzy selector, and connects to the selected target with the system `ssh` binary. When you run `s <target>`, it records that target first, updates recency and usage count, then runs `ssh <target>`.

## Prerequisites

- `ssh` available in `PATH`

## Build

```bash
cargo build --release
```

The binary will be at:

```bash
target/release/s
```

## Usage

```bash
s
```

Show recent SSH targets through the embedded selector, then connect to the selected one.

```bash
s prod
```

Record `prod` in local history, update recency, then run `ssh prod`.

```bash
s user@example.com
```

Record `user@example.com` in local history, update recency, then run `ssh user@example.com`.

## History Storage

History is stored as JSON at:

- `$XDG_CONFIG_HOME/s/history.json` when `XDG_CONFIG_HOME` is set
- otherwise `~/.config/s/history.json`

Each entry has this shape:

```json
{
  "target": "user@host",
  "last_used_at": 1700000000,
  "use_count": 3
}
```

## Notes

- History is de-duplicated by exact target string.
- Entries are shown by most recent use first.
- `s` does not manage passwords, keys, `ssh-agent`, or SSH protocol details.
- `s` uses the system `ssh` binary and an embedded Rust fuzzy selector.
