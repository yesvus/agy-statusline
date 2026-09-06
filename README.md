# agy-statusline

An ultra-fast, zero-overhead custom statusline binary for [Google Antigravity](https://github.com/google) CLI (`agy`), written in Rust.

```text
Gemini 3.8 Flash (High) · lumen · ctx 12% (150k/1M)
5H 85%(4h24m) | 3P 100%(4h45m) · W 11%(4d6h) | 3P 14%(5d5h)
```

## Highlights

- **Sub-Millisecond Execution (~1 ms):** Replaces heavy Python/Bash statusline scripts, eliminating CPU spikes during rapid terminal refreshes.
- **Project-Specific Context:** Reads workspace directory, active model, and conversation context window usage directly from the active session payload. No cross-project collision or context leakage between terminals.
- **Account-Wide Quota Sync:** Synchronizes 5-hour and Weekly Gemini/3P quota across all running `agy` sessions via `/dev/shm` IPC with safe file-locking (`flock`). When any project consumes tokens or receives quota updates, all active terminals reflect the true account state.
- **Live Reset Countdowns:** Automatically computes real-time countdowns to quota resets (`XhYm`, `XdYh`).
- **ANSI Colored Output:** Intuitive green/yellow/red color thresholds for both token consumption and remaining quota.

## Installation

### From Source

Ensure you have Rust installed (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`):

```bash
git clone https://github.com/yesvus/agy-statusline.git
cd agy-statusline
cargo build --release
cp target/release/agy-statusline ~/.local/bin/
```

Or install directly with cargo:

```bash
cargo install --path .
```

## Configuration

Add or update the `statusLine` configuration in your `~/.gemini/antigravity-cli/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "/home/yesvus/.local/bin/agy-statusline",
    "enabled": true
  }
}
```

*(Alternatively, you can call it from a shell wrapper script with `exec ~/.local/bin/agy-statusline "$@"`).*

## How It Works

1. `agy` streams JSON session metrics to the statusline command on standard input (`stdin`).
2. `agy-statusline` extracts:
   - **Row 1:** Model name, current project directory, and context window used (`used_percentage` and token ratio).
   - **Row 2:** Gemini 5h, 3P 5h, Gemini Weekly, and 3P Weekly quotas.
3. Quotas are synchronized via `/dev/shm/agy_quota.<UID>.json`. The lowest remaining fraction for the active quota window is preserved across all running instances so sessions never race or overwrite with stale values.

## License

MIT © [yesvus](https://github.com/yesvus)
