use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

use crate::config::AppConfig;
use crate::runtime::find_file_recursive;

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
}

fn find_bench_binary(runtime_dir: &std::path::Path) -> Option<PathBuf> {
    let target = if cfg!(target_os = "windows") {
        "llama-bench.exe"
    } else {
        "llama-bench"
    };
    find_file_recursive(runtime_dir, target, 3)
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
    if let Some(ctx) = n_ctx {
        if ctx > 0 {
            cmd.arg("-c").arg(ctx.to_string());
        }
    }
    if let Some(ngl) = n_gpu_layers {
        cmd.arg("-ngl").arg(ngl.to_string());
    }

    // Suppress console window on Windows
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        cmd.output(),
    )
    .await
    .context("Benchmark timed out after 120s")?
    .context("Failed to spawn llama-bench")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Try to parse CSV: header includes "pp_tps" and "tg_tps" or similar
    // Fallback to scanning for "tg=" / "pp=" in text
    let mut pp_tps: Option<f64> = None;
    let mut tg_tps: Option<f64> = None;
    let mut status = if output.status.success() {
        "ok"
    } else {
        "error"
    }
    .to_string();

    // CSV parse: first line header, second line data
    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.lines() {
        let lower = line.to_lowercase();
        if lower.contains("tg") && lower.contains("pp") && pp_tps.is_none() {
            // Try to find numbers like "tg=7.4" or "pp=122.0"
            if let Some(v) = extract_metric(line, "tg") {
                tg_tps = Some(v);
            }
            if let Some(v) = extract_metric(line, "pp") {
                pp_tps = Some(v);
            }
        }
    }
    // If CSV header present, try more structured parse
    if pp_tps.is_none() || tg_tps.is_none() {
        if let Some((pp, tg)) = parse_csv_throughput(&stdout) {
            if pp_tps.is_none() {
                pp_tps = Some(pp);
            }
            if tg_tps.is_none() {
                tg_tps = Some(tg);
            }
        }
    }

    if !output.status.success() {
        status = format!("bench exited {}", output.status);
    } else if pp_tps.is_none() && tg_tps.is_none() {
        status = "ok (no throughput parsed)".to_string();
    }

    Ok(BenchResult {
        model_path,
        n_prompt: p,
        n_gen: n,
        n_threads,
        batch_size: batch_size.unwrap_or(0),
        ubatch_size: ubatch_size.unwrap_or(0),
        n_ctx: n_ctx.unwrap_or(0),
        n_gpu_layers: n_gpu_layers.unwrap_or(-1),
        pp_tps,
        tg_tps,
        status,
        raw_stdout: stdout,
        raw_stderr: stderr,
    })
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

fn parse_csv_throughput(csv: &str) -> Option<(f64, f64)> {
    let mut lines = csv.lines();
    let header = lines.next()?.to_lowercase();
    // Find column indices for pp and tg
    let headers: Vec<String> = header.split(',').map(|s| s.trim().to_lowercase()).collect();
    let pp_idx = headers.iter().position(|h| h.contains("pp") && h.contains("tps"))?;
    let tg_idx = headers.iter().position(|h| h.contains("tg") && h.contains("tps"))?;
    // Next non-empty line is data
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
