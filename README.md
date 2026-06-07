<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/celer-banner-dark.png">
  <img src="assets/celer-banner-light.png" alt="celer">
</picture>

Parallel git repo puller with live progress, dirty detection, and safe defaults.

---

## Features

- **Parallel Pulls** — Pull dozens of repos at once using tokio async workers
- **Live Progress** — Watch real-time status lines for every repo as they update
- **Dirty Detection** — Automatically skips repos with uncommitted changes so you never lose work
- **Stash Mode** — Optional `--stash` to safely stash, pull, and pop your changes
- **Dry Run** — Preview what would be pulled without changing anything
- **Filter** — Include or exclude specific repos by name
- **Clear Summary** — Final breakdown of updated, skipped, failed, and stashed repos

---

## Install

```bash
cargo install --path .
```

Requires `cargo` and `git`.

---

## Usage

```bash
celer [OPTIONS]
```

### Flags

| Flag | Default | Description |
|------|---------|-------------|
| `-p, --path [<PATH>]` | `.` | Root directory to scan |
| `-d, --depth [<DEPTH>]` | `3` | Directory levels to search |
| `--dry-run` | — | Report status without pulling |
| `--ignore <NAMES>` | — | Comma-separated repo names to skip |
| `--only <NAMES>` | — | Comma-separated repo names to pull |
| `--stash` | — | Stash dirty repos before pulling, pop after |

### Examples

```bash
# Pull everything in current directory (depth 3)
celer

# Scan a different root
celer -p ~/projects

# Search deeper
celer -d 5

# Pull only specific repos
celer --only backend,frontend

# Skip specific repos
celer --ignore old-project,experiments

# Stash dirty repos, pull, then restore
celer --stash

# Preview without pulling
celer --dry-run
```

---

## Live Status

Each repo gets a live line:

```
repo-name | branch | [TAG] message
```

| Tag | Meaning |
|-----|---------|
| `[...]` | Checking branch and dirty status |
| `[PULL]` | Pulling now |
| `[OK]` | Already up to date |
| `[UPD]` | New commits pulled |
| `[SKIP]` | Skipped — uncommitted changes |
| `[STASH]` | Stashing before pull |
| `[DRY]` | Dry run, no changes |
| `[ERR]` | Pull or git command failed |
| `[WARN]` | Warning (e.g. stash failed) |

---

## Summary

At the end you get:

```
--------------------------------------------------
Summary
[UPD]   Updated           4
[OK]    Already up to date 12
[STASH] Stashed & updated  2
[SKIP]  Skipped / warned   1
[ERR]   Failed             0
--------------------------------------------------
```

If anything fails or is skipped, a **Details** section follows with the exact repo, branch, and reason.

Exits with code `1` if any pull fails.

---

## Safety

- Dirty repos are **skipped by default** — no blind pulls
- Use `--stash` to opt into stashing; runs `git stash push -m "celer-auto-stash"`, pulls, then `git stash pop`
- Detached HEAD repos are caught and marked `[ERR]`

---
