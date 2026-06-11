use anyhow::{Context, Result};
use clap::Parser;
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Semaphore;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "boreas")]
#[command(about = "Parallel git repo puller with live progress")]
struct Cli {
    #[arg(short, long, default_value = ".", default_missing_value = ".", num_args = 0..=1)]
    path: PathBuf,

    #[arg(short, long, default_value = "3", default_missing_value = "3", num_args = 0..=1)]
    depth: usize,

    #[arg(long)]
    dry_run: bool,

    #[arg(long, value_delimiter = ',')]
    ignore: Vec<String>,

    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,

    #[arg(long)]
    stash: bool,

    #[arg(long)]
    version: bool,
}

#[derive(Debug, Clone)]
enum RepoOutcome {
    Updated,
    UpToDate,
    DirtySkipped,
    Failed(String),
    StashedAndUpdated,
    StashFailed(String),
    DryRun { dirty: bool },
}

struct RepoResult {
    name: String,
    branch: String,
    outcome: RepoOutcome,
}

fn find_git_repos(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    for entry in WalkDir::new(root)
        .max_depth(depth)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() && entry.file_name() == ".git" {
            if let Some(parent) = entry.path().parent() {
                repos.push(parent.to_path_buf());
            }
        }
    }

    // Keep only outermost repos: if one repo is inside another, discard the inner one
    repos.sort();
    let mut outermost = Vec::new();
    for repo in repos {
        let is_nested = outermost.iter().any(|outer| repo.starts_with(outer));
        if !is_nested {
            outermost.push(repo);
        }
    }
    outermost
}

fn repo_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            path.canonicalize()
                .ok()
                .and_then(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown".to_string())
        })
}

async fn git_branch(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["branch", "--show-current"])
        .output()
        .await
        .context("failed to run git branch")?;

    if !output.status.success() {
        anyhow::bail!(
            "git branch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        anyhow::bail!("no current branch (detached HEAD?)");
    }
    Ok(branch)
}

async fn is_dirty(repo: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .await
        .context("failed to run git status")?;

    if !output.status.success() {
        anyhow::bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(!output.stdout.is_empty())
}

async fn git_stash(repo: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["stash", "push", "-m", "boreas-auto-stash"])
        .output()
        .await
        .context("failed to run git stash")?;

    if !output.status.success() {
        anyhow::bail!("git stash failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

async fn git_stash_pop(repo: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["stash", "pop"])
        .output()
        .await
        .context("failed to run git stash pop")?;

    if !output.status.success() {
        anyhow::bail!(
            "git stash pop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn set_status(pb: &ProgressBar, name: &str, branch: &str, icon: &str, status: &str, color: Color) {
    let line = format!(
        "{} | {} | {} {}",
        name.bold(),
        branch.cyan(),
        icon,
        status.color(color)
    );
    pb.set_message(line);
}

async fn pull_repo(
    repo_path: PathBuf,
    display_name: String,
    mp: MultiProgress,
    dry_run: bool,
    stash: bool,
) -> RepoResult {
    let name = display_name;

    let pb = mp.add(ProgressBar::new_spinner());
    pb.enable_steady_tick(Duration::from_millis(100));
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["|", "/", "-", "\\"]),
    );

    let branch = match git_branch(&repo_path).await {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("branch error: {e}");
            set_status(&pb, &name, "?", "[ERR]", &msg, Color::Red);
            pb.finish();
            return RepoResult {
                name,
                branch: "?".into(),
                outcome: RepoOutcome::Failed(msg),
            };
        }
    };

    set_status(&pb, &name, &branch, "[...]", "checking...", Color::Yellow);

    let dirty = match is_dirty(&repo_path).await {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("dirty check failed: {e}");
            set_status(&pb, &name, &branch, "[ERR]", &msg, Color::Red);
            pb.finish();
            return RepoResult {
                name,
                branch,
                outcome: RepoOutcome::Failed(msg),
            };
        }
    };

    if dry_run {
        let status = if dirty { "dirty" } else { "clean" };
        let color = if dirty { Color::Yellow } else { Color::Green };
        set_status(
            &pb,
            &name,
            &branch,
            "[DRY]",
            &format!("dry-run ({status})"),
            color,
        );
        pb.finish();
        return RepoResult {
            name,
            branch: branch.clone(),
            outcome: RepoOutcome::DryRun {
                dirty,
            },
        };
    }

    if dirty {
        if stash {
            set_status(&pb, &name, &branch, "[STASH]", "stashing...", Color::Blue);
            if let Err(e) = git_stash(&repo_path).await {
                let msg = format!("stash failed: {e}");
                set_status(&pb, &name, &branch, "[WARN]", &msg, Color::Yellow);
                pb.finish();
                return RepoResult {
                    name,
                    branch,
                    outcome: RepoOutcome::StashFailed(msg),
                };
            }
        } else {
            set_status(
                &pb,
                &name,
                &branch,
                "[SKIP]",
                "dirty -- skipped",
                Color::Yellow,
            );
            pb.finish();
            return RepoResult {
                name,
                branch,
                outcome: RepoOutcome::DirtySkipped,
            };
        }
    }

    set_status(&pb, &name, &branch, "[PULL]", "pulling...", Color::Cyan);

    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(["pull"])
        .output()
        .await;

    let (outcome, icon, status, color) = match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            let combined = format!("{}{}", stdout, stderr);
            if combined.contains("Already up to date") || combined.contains("Already up-to-date") {
                (
                    RepoOutcome::UpToDate,
                    "[OK]".to_string(),
                    "already up to date".to_string(),
                    Color::Green,
                )
            } else {
                (RepoOutcome::Updated, "[UPD]".to_string(), "updated".to_string(), Color::Green)
            }
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            (RepoOutcome::Failed(err.clone()), "[ERR]".to_string(), err, Color::Red)
        }
        Err(e) => (
            RepoOutcome::Failed(e.to_string()),
            "[ERR]".to_string(),
            e.to_string(),
            Color::Red,
        ),
    };

    let final_outcome = if dirty && stash {
        match &outcome {
            RepoOutcome::Updated => RepoOutcome::StashedAndUpdated,
            RepoOutcome::UpToDate => RepoOutcome::StashedAndUpdated,
            _ => outcome.clone(),
        }
    } else {
        outcome.clone()
    };

    let status_text = match &final_outcome {
        RepoOutcome::StashedAndUpdated => "updated (stash -> pull -> pop)".to_string(),
        _ => status,
    };
    set_status(&pb, &name, &branch, &icon, &status_text, color);


    if dirty && stash {
        if matches!(
            final_outcome,
            RepoOutcome::StashedAndUpdated | RepoOutcome::UpToDate
        ) {
            if let Err(e) = git_stash_pop(&repo_path).await {
                let msg = format!("stash pop failed: {e}");
                set_status(&pb, &name, &branch, "[WARN]", &msg, Color::Yellow);
                pb.finish();
                return RepoResult {
                    name,
                    branch,
                    outcome: RepoOutcome::Failed(msg),
                };
            }
        }
    }

    pb.finish();
    RepoResult {
        name,
        branch,
        outcome: final_outcome,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.version {
        println!("boreas {}", env!("CARGO_PKG_VERSION").blue());
        return Ok(());
    }

    let ignore_set: HashSet<String> = cli.ignore.into_iter().collect();
    let only_set: HashSet<String> = cli.only.into_iter().collect();

    let repos = find_git_repos(&cli.path, cli.depth);

    let mut filtered: Vec<PathBuf> = repos
        .into_iter()
        .filter(|r| {
            let name = repo_name(r);
            if !ignore_set.is_empty() && ignore_set.contains(&name) {
                return false;
            }
            if !only_set.is_empty() && !only_set.contains(&name) {
                return false;
            }
            true
        })
        .collect();

    filtered.sort();

    let total = filtered.len();
    if total == 0 {
        println!("[INFO] No git repositories found.");
        return Ok(());
    }

    println!(
        "[INFO] Found {} repo(s) under {} (depth={})\n",
        total.to_string().bold(),
        cli.path.display().to_string().cyan(),
        cli.depth
    );

    let mp = MultiProgress::new();
    let concurrency = std::cmp::max(4, num_cpus::get());
    let sem = std::sync::Arc::new(Semaphore::new(concurrency));

    let mut tasks = Vec::with_capacity(total);
    for repo in filtered {
        let display_name = repo
            .strip_prefix(&cli.path)
            .unwrap_or(&repo)
            .to_string_lossy()
            .to_string();
        let permit = sem.clone().acquire_owned().await.unwrap();
        let mp = mp.clone();
        let dry = cli.dry_run;
        let stash = cli.stash;
        let handle = tokio::spawn(async move {
            let _permit = permit;
            pull_repo(repo, display_name, mp, dry, stash).await
        });
        tasks.push(handle);
    }

    let mut results = Vec::with_capacity(tasks.len());
    for t in tasks {
        results.push(t.await?);
    }

    let mut updated = 0;
    let mut up_to_date = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut stashed = 0;

    for r in &results {
        match &r.outcome {
            RepoOutcome::Updated => updated += 1,
            RepoOutcome::UpToDate => up_to_date += 1,
            RepoOutcome::DirtySkipped => skipped += 1,
            RepoOutcome::Failed(_) => failed += 1,
            RepoOutcome::StashedAndUpdated => stashed += 1,
            RepoOutcome::StashFailed(_) => failed += 1,
            RepoOutcome::DryRun { dirty } => {
                if *dirty {
                    skipped += 1
                } else {
                    updated += 1
                }
            }
        }
    }

    println!("\n{}", "-".repeat(50).dimmed());
    println!("{}", "Summary".bold().underline());
    println!("[UPD] Updated {}", updated.to_string().green().bold());
    println!(
        "[OK]  Already up to date {}",
        up_to_date.to_string().green().bold()
    );
    println!(
        "[STASH] Stashed & updated {}",
        stashed.to_string().blue().bold()
    );
    println!(
        "[SKIP] Skipped / warned {}",
        skipped.to_string().yellow().bold()
    );
    println!("[ERR] Failed {}", failed.to_string().red().bold());
    println!("{}", "-".repeat(50).dimmed());

    if failed > 0 || skipped > 0 {
        println!("\n{}", "Details".bold().underline());
        for r in results {
            match r.outcome {
                RepoOutcome::Failed(msg) | RepoOutcome::StashFailed(msg) => {
                    println!(
                        "[ERR] {} ({}): {}",
                        r.name.bold(),
                        r.branch.cyan(),
                        msg.red()
                    );
                }
                RepoOutcome::DirtySkipped => {
                    println!(
                        "[SKIP] {} ({}): {}",
                        r.name.bold(),
                        r.branch.cyan(),
                        "uncommitted changes -- skipped".yellow()
                    );
                }
                _ => {}
            }
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
