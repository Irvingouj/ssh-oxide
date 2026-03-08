# s

`s` is a tiny Rust CLI that wraps the system OpenSSH client, keeps a local history of recently used SSH targets, and can install your local public key on a remote server.

When you run `s` with no arguments, it loads your saved targets, opens an embedded fuzzy selector, and connects to the selected target with the system `ssh` binary. When you run `s <target>`, it records that target first, updates recency and usage count, then runs `ssh <target>`.

When you run `s add-key`, it shows recent targets interactively and installs your local public key on the selected host. When you run `s add-key <target>`, it installs the public key directly on that target. `s` never copies a private key.

## Prerequisites

- `ssh` available in `PATH`
- `ssh-copy-id` is optional; if present, `s add-key` prefers it and otherwise falls back to a standard `authorized_keys` append flow over `ssh`

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

```bash
s add-key
```

Show recent SSH targets through the embedded selector, then install your local public key on the selected host.

```bash
s add-key prod
```

Install your local public key on `prod`.

```bash
s add-key -p 2222 root@1.2.3.4
```

Install your local public key on `root@1.2.3.4` using SSH port `2222`.

```bash
s add-key --key ~/.ssh/custom.pub user@example.com
```

Install the explicitly selected public key on `user@example.com`.

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
- `s add-key` records the target in history too, so recently provisioned hosts stay easy to reach.
- `s` does not manage passwords, private keys, `ssh-agent`, or SSH protocol details.
- `s` uses the system `ssh` binary and an embedded Rust fuzzy selector.
- For `add-key`, `s` prefers `ssh-copy-id` when available and otherwise falls back to a remote `authorized_keys` append command.
