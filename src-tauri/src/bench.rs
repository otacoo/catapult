use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::Command;

use crate::config::AppConfig;
use crate::runtime::find_file_recursive;

/// Only one llama-bench may run at a time: two benches loading the same model
/// onto the same GPU contend for memory/compute and can starve each other past
/// the timeout. This guards against double-invocation from any entry point.
static BENCH_RUNNING: AtomicBool = AtomicBool::new(false);

struct BenchRunGuard;

impl Drop for BenchRunGuard {
    fn drop(&mut self) {
        BENCH_RUNNING.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchResult {
    pub model_path: String,
    pub n_prompt: u32,
    pub n_gen: u32,
    pub n_threads: Option<i32>,
    pub batch_size: u32,
    pub ubatch_size: u32,
    pub n_ctx: u32,
    pub n_gpu_layers: i32,
    /// Prompt processing t/s
    pub pp_tps: Option<f64>,
    /// Token generation t/s
    pub tg_tps: Option<f64>,
    pub status: String,
    pub raw_stdout: String,
    pub raw_stderr: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub model_name: String,
    #[serde(default)]
    pub build_number: Option<u32>,
    #[serde(default)]
    pub build_commit: Option<String>,
}

fn find_bench_binary(runtime_dir: &std::path::Path) -> Option<PathBuf> {
    let target = if cfg!(target_os = "windows") {
        "llama-bench.exe"
    } else {
        "llama-bench"
    };
    find_file_recursive(runtime_dir, target, 3)
}

pub fn bench_results_path() -> Result<PathBuf> {
    let data_dir = dirs::data_dir().ok_or_else(|| anyhow::anyhow!("Cannot find data dir"))?;
    Ok(data_dir.join("catapult").join("bench_results.json"))
}

pub fn load_bench_results() -> Result<Vec<BenchResult>> {
    let path = bench_results_path()?;
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

pub fn append_bench_result(result: &BenchResult) -> Result<()> {
    let path = bench_results_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut results = load_bench_results().unwrap_or_default();
    results.push(result.clone());
    if results.len() > 200 {
        results.drain(0..results.len() - 200);
    }
    let json = serde_json::to_string_pretty(&results)?;
    std::fs::write(path, json)?;
    Ok(())
}

pub fn clear_bench_results() -> Result<()> {
    let path = bench_results_path()?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Run a quick `llama-bench` with the current ServerConfig-ish flags.
/// Keeps it to 1 rep by default for responsiveness (like --quick).
pub async fn run_quick_bench(
    app_config: &AppConfig,
    model_path: String,
    n_prompt: Option<u32>,
    n_gen: Option<u32>,
    n_threads: Option<i32>,
    batch_size: Option<u32>,
    ubatch_size: Option<u32>,
    n_ctx: Option<u32>,
    n_gpu_layers: Option<i32>,
) -> Result<BenchResult> {
    if BENCH_RUNNING.swap(true, Ordering::SeqCst) {
        anyhow::bail!("A benchmark is already running");
    }
    let _run_guard = BenchRunGuard;

    let runtime_info = crate::runtime::get_runtime_info(app_config)?;
    let runtime_dir = app_config.runtime_dir().context("No runtime installed")?;
    let bench_bin = find_bench_binary(&runtime_dir)
        .ok_or_else(|| anyhow::anyhow!("llama-bench not found in {}", runtime_dir.display()))?;

    let p = n_prompt.unwrap_or(512);
    let n = n_gen.unwrap_or(128);

    let mut cmd = Command::new(&bench_bin);
    cmd.arg("-m").arg(&model_path);
    cmd.arg("-p").arg(p.to_string());
    cmd.arg("-n").arg(n.to_string());
    cmd.arg("-r").arg("1"); // quick: 1 repetition
    cmd.arg("--output").arg("csv");
    // Use current config if provided, else let bench defaults handle it
    if let Some(t) = n_threads {
        cmd.arg("-t").arg(t.to_string());
    }
    if let Some(b) = batch_size {
        cmd.arg("-b").arg(b.to_string());
    }
    if let Some(ub) = ubatch_size {
        // llama-bench flag for ubatch is often -ub or --ubatch-size; try both
        // Prefer --ubatch-size if supported, fallback to -ub via raw args.
        cmd.arg("--ubatch-size").arg(ub.to_string());
    }
    // bench has no -c/--ctx-size flag – context is derived from model/prompt.
    // Passing -c makes it exit 1 with help. Only pass ngl.
    if let Some(ngl) = n_gpu_layers {
        cmd.arg("-ngl").arg(ngl.to_string());
    }

    // Suppress console window on Windows
    #[cfg(target_os = "windows")]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    // kill_on_drop ensures llama-bench never survives as an orphan: if the
    // wait future is dropped (timeout below), the child is killed instead of
    // silently running on and consuming GPU/CPU resources.
    cmd.kill_on_drop(true);

    let child = cmd.spawn().context("Failed to spawn llama-bench")?;
    // Generous limit: model loading alone can take minutes on large models.
    // On timeout the future (and the child, via kill_on_drop) is dropped.
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        child.wait_with_output(),
    )
    .await
    {
        Ok(res) => res.context("Failed to run llama-bench")?,
        Err(_) => {
            anyhow::bail!("Benchmark timed out after 300s — llama-bench was stopped");
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let mut pp_tps: Option<f64> = None;
    let mut tg_tps: Option<f64> = None;
    let mut status = if output.status.success() {
        "ok"
    } else {
        "error"
    }
    .to_string();

    // Try structured CSV parse first (handles llama-bench --output csv where pp/tg
    // are distinguished by n_prompt/n_gen and throughput is in avg_ts)
    if let Some((pp, tg)) = parse_llama_bench_csv(&stdout).or_else(|| parse_llama_bench_csv(&stderr)) {
        pp_tps = Some(pp);
        tg_tps = Some(tg);
    }
    // Fallback: markdown table (| pp512 | 540.99 ± ... | / | tg128 | 35.54 |)
    if pp_tps.is_none() || tg_tps.is_none() {
        let md_pp = parse_llama_bench_md(&stdout).or_else(|| parse_llama_bench_md(&stderr));
        if let Some((pp, tg)) = md_pp {
            if pp_tps.is_none() { pp_tps = Some(pp); }
            if tg_tps.is_none() { tg_tps = Some(tg); }
        }
    }
    // Last fallback: scan for tg= / pp= fragments
    if pp_tps.is_none() || tg_tps.is_none() {
        let combined = format!("{}\n{}", stdout, stderr);
        for line in combined.lines() {
            let lower = line.to_lowercase();
            if lower.contains("tg") && lower.contains("pp") && pp_tps.is_none() {
                if let Some(v) = extract_metric(line, "tg") { tg_tps = Some(v); }
                if let Some(v) = extract_metric(line, "pp") { pp_tps = Some(v); }
            }
        }
    }

    if !output.status.success() {
        status = format!("bench exited {}", output.status);
    } else if pp_tps.is_none() && tg_tps.is_none() {
        status = "ok (no throughput parsed)".to_string();
    }

    let mut build_number = runtime_info.build;
    let mut build_commit: Option<String> = None;
    if let Some((bn, bc)) = parse_bench_build(&stdout).or_else(|| parse_bench_build(&stderr)) {
        build_number = Some(bn);
        if !bc.is_empty() {
            build_commit = Some(bc);
        }
    }

    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let model_name = std::path::Path::new(&model_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let result = BenchResult {
        model_path: model_path.clone(),
        n_prompt: p,
        n_gen: n,
        n_threads,
        batch_size: batch_size.unwrap_or(0),
        ubatch_size: ubatch_size.unwrap_or(0),
        n_ctx: n_ctx.unwrap_or(0),
        n_gpu_layers: n_gpu_layers.unwrap_or(-1),
        pp_tps,
        tg_tps,
        status: status.clone(),
        raw_stdout: stdout.clone(),
        raw_stderr: stderr.clone(),
        timestamp,
        model_name,
        build_number,
        build_commit,
    };
    let _ = append_bench_result(&result);
    Ok(result)
}

fn extract_metric(line: &str, key: &str) -> Option<f64> {
    // Look for "key=12.34" or "key: 12.34" or "key 12.34"
    let lower = line.to_lowercase();
    let needle = format!("{}=", key);
    if let Some(idx) = lower.find(&needle) {
        let rest = &line[idx + needle.len()..];
        let num_str: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == 'e' || *c == 'E')
            .collect();
        if let Ok(v) = num_str.parse::<f64>() {
            return Some(v);
        }
    }
    // Try "key: value"
    let needle2 = format!("{}:", key);
    if let Some(idx) = lower.find(&needle2) {
        let rest = &line[idx + needle2.len()..];
        let trimmed = rest.trim_start_matches(|c: char| c == ' ' || c == '=');
        let num_str: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' )
            .collect();
        if let Ok(v) = num_str.parse::<f64>() {
            return Some(v);
        }
    }
    None
}

fn parse_llama_bench_csv(csv: &str) -> Option<(f64, f64)> {
    // llama-bench --output csv has header with n_prompt,n_gen,avg_ts,…
    // pp row: n_prompt=512 n_gen=0, tg row: n_prompt=0 n_gen=128, avg_ts = throughput
    let mut lines = csv.lines();
    let header = lines.next()?.to_lowercase();
    let headers: Vec<String> = header.split(',').map(|s| s.trim().to_lowercase()).collect();
    let n_prompt_idx = headers.iter().position(|h| h == "n_prompt")?;
    let n_gen_idx = headers.iter().position(|h| h == "n_gen")?;
    let avg_ts_idx = headers.iter().position(|h| h == "avg_ts")?;
    let mut pp: Option<f64> = None;
    let mut tg: Option<f64> = None;
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Naive CSV split – values are simple, quoted but no commas inside
        let cols: Vec<String> = line.split(',').map(|c| c.trim().trim_matches('"').to_string()).collect();
        if cols.len() <= n_prompt_idx.max(n_gen_idx).max(avg_ts_idx) {
            continue;
        }
        let np: u32 = cols[n_prompt_idx].parse().ok()?;
        let ng: u32 = cols[n_gen_idx].parse().ok()?;
        let ts: f64 = cols[avg_ts_idx].parse().ok()?;
        if np > 0 && ng == 0 {
            pp = Some(ts);
        } else if np == 0 && ng > 0 {
            tg = Some(ts);
        }
        if pp.is_some() && tg.is_some() {
            return Some((pp.unwrap(), tg.unwrap()));
        }
    }
    match (pp, tg) {
        (Some(p), Some(t)) => Some((p, t)),
        _ => None,
    }
}

fn parse_llama_bench_md(md: &str) -> Option<(f64, f64)> {
    // Markdown table: | ... | pp512 | 540.99 ± 0.00 |  and | ... | tg128 | 35.54 ± ... |
    let mut pp: Option<f64> = None;
    let mut tg: Option<f64> = None;
    for line in md.lines() {
        // Look for pipe-separated cells
        if line.contains('|') {
            let cells: Vec<String> = line.split('|').map(|c| c.trim().to_string()).collect();
            for (i, cell) in cells.iter().enumerate() {
                let cl = cell.to_lowercase();
                if cl.starts_with("pp") {
                    // Next cell should contain "540.99 ±"
                    if let Some(next) = cells.get(i + 1) {
                        if let Some(v) = next.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) {
                            pp = Some(v);
                        }
                    }
                } else if cl.starts_with("tg") {
                    if let Some(next) = cells.get(i + 1) {
                        if let Some(v) = next.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()) {
                            tg = Some(v);
                        }
                    }
                }
            }
        }
        if pp.is_some() && tg.is_some() {
            return Some((pp.unwrap(), tg.unwrap()));
        }
    }
    match (pp, tg) {
        (Some(p), Some(t)) => Some((p, t)),
        _ => None,
    }
}

fn parse_bench_build(csv: &str) -> Option<(u32, String)> {
    let mut lines = csv.lines();
    let header = lines.next()?.to_lowercase();
    let headers: Vec<String> = header.split(',').map(|s| s.trim().to_lowercase()).collect();
    let bn_idx = headers.iter().position(|h| h == "build_number")?;
    let bc_idx = headers.iter().position(|h| h == "build_commit")?;
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<String> = line.split(',').map(|c| c.trim().trim_matches('"').to_string()).collect();
        if cols.len() <= bn_idx.max(bc_idx) {
            continue;
        }
        if let Ok(bn) = cols[bn_idx].parse::<u32>() {
            if bn != 0 {
                return Some((bn, cols[bc_idx].clone()));
            }
        }
    }
    None
}

#[allow(dead_code)]
fn parse_csv_throughput(csv: &str) -> Option<(f64, f64)> {
    let mut lines = csv.lines();
    let header = lines.next()?.to_lowercase();
    let headers: Vec<String> = header.split(',').map(|s| s.trim().to_lowercase()).collect();
    let pp_idx = headers.iter().position(|h| h.contains("pp") && h.contains("tps"))?;
    let tg_idx = headers.iter().position(|h| h.contains("tg") && h.contains("tps"))?;
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() <= pp_idx.max(tg_idx) {
            continue;
        }
        let pp = cols[pp_idx].trim().parse::<f64>().ok()?;
        let tg = cols[tg_idx].trim().parse::<f64>().ok()?;
        return Some((pp, tg));
    }
    None
}
