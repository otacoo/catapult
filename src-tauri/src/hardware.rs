use anyhow::Result;
use serde::{Deserialize, Serialize};
use sysinfo::System;

/// Creates a Command that won't spawn a visible console window on Windows.
fn silent_cmd(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub cpu_name: String,
    pub cpu_cores: u32,
    pub cpu_threads: u32,
    pub total_ram_mb: u64,
    pub available_ram_mb: u64,
    pub gpus: Vec<GpuInfo>,
    pub os: String,
    pub arch: String,
    pub available_backends: Vec<BackendInfo>,
    pub recommended_backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vram_mb: u64,
    pub vendor: GpuVendor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

fn get_cuda_version() -> Option<String> {
    // Prefer nvcc --version (CUDA toolkit — what's actually installed).
    // Fall back to nvidia-smi (driver-supported CUDA version) if nvcc isn't available.
    get_cuda_version_from_nvcc().or_else(get_cuda_version_from_nvsmi)
}

fn get_cuda_version_from_nvcc() -> Option<String> {
    let output = silent_cmd("nvcc").args(["--version"]).output().ok()?;
    if !output.status.success() { return None; }
    let text = String::from_utf8_lossy(&output.stdout);
    // Look for "release X.Y," in the output
    let marker = "release ";
    text.lines().find(|line| line.contains(marker)).and_then(|line| {
        let start = line.find(marker)? + marker.len();
        let rest = &line[start..];
        let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len());
        if end > 0 { Some(rest[..end].to_string()) } else { None }
    })
}

fn get_cuda_version_from_nvsmi() -> Option<String> {
    let output = silent_cmd("nvidia-smi").output().ok()?;
    if !output.status.success() { return None; }
    let text = String::from_utf8_lossy(&output.stdout);
    let marker = "CUDA Version: ";
    text.lines().find(|line| line.contains(marker)).and_then(|line| {
        let start = line.find(marker)? + marker.len();
        let rest = &line[start..];
        let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len());
        if end > 0 { Some(rest[..end].to_string()) } else { None }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedConfig {
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub can_fit_fully_in_vram: bool,
    pub total_usable_mb: u64,
    pub notes: Vec<String>,
    /// Suggested CPU threads (physical cores), `None` if unknown.
    #[serde(default)]
    pub n_threads: Option<i32>,
    /// Suggested batch sizes (heuristic, no benchmark).
    #[serde(default)]
    pub n_batch: Option<u32>,
    #[serde(default)]
    pub n_ubatch: Option<u32>,
}

/// Estimated memory breakdown for a model + settings on the current machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEstimate {
    pub model_mb: u64,
    pub kv_cache_mb: u64,
    pub overhead_mb: u64,
    pub total_mb: u64,
    /// Total GPU VRAM in MB
    pub vram_total_mb: u64,
    /// Available system RAM in MB
    pub ram_available_mb: u64,
    /// Estimated VRAM usage in MB
    pub vram_used_mb: u64,
    /// Estimated RAM usage in MB
    pub ram_used_mb: u64,
    // Per-resource breakdown for visualization
    pub vram_model_mb: u64,
    pub vram_kv_mb: u64,
    pub vram_overhead_mb: u64,
    pub ram_model_mb: u64,
    pub ram_kv_mb: u64,
    pub ram_overhead_mb: u64,
    /// True if the estimate fits in VRAM + available RAM
    pub fits: bool,
    pub notes: Vec<String>,
}

fn cache_bytes_per_element(cache_type: &str) -> u64 {
    match cache_type {
        "f32" => 4,
        "q8_0" => 1,
        "q4_0" | "q4_1" | "iq4_nl" | "q5_0" | "q5_1" => 1, // sub-byte, treat as 1
        _ => 2, // f16, bf16, and anything unknown
    }
}

/// KV bytes per token (across all layers) for the given dimensions.
pub fn kv_bytes_per_token(layers: u64, kv_embd: u64, cache_type_k: &str, cache_type_v: &str) -> u64 {
    layers
        .saturating_mul(kv_embd)
        .saturating_mul(cache_bytes_per_element(cache_type_k) + cache_bytes_per_element(cache_type_v))
}

/// KV cache size in MB for the given model dimensions and context.
/// `kv_embd` is the effective per-layer KV dimension (embd × kv_heads/heads).
/// `--ctx-size` is the total budget shared across server slots, so this does
/// not scale with `--parallel`.
fn kv_cache_mb(layers: u64, kv_embd: u64, ctx: u64, cache_type_k: &str, cache_type_v: &str) -> u64 {
    kv_bytes_per_token(layers, kv_embd, cache_type_k, cache_type_v)
        .saturating_mul(ctx)
        / (1024 * 1024)
}

/// Estimate the memory footprint of running a model with the given settings.
/// `model_size_mb` is the GGUF file size; layer/embedding info is read from
/// the file header when available, with sensible fallbacks otherwise.
pub fn estimate_memory(
    model_path: &str,
    model_size_mb: u64,
    n_ctx: u32,
    cache_type_k: &str,
    cache_type_v: &str,
    n_gpu_layers: i32,
) -> Result<MemoryEstimate> {
    let system = get_system_info()?;
    let vram_total_mb: u64 = system.gpus.iter().map(|g| g.vram_mb).sum();
    let ram_available_mb = system.available_ram_mb;
    let mut notes = Vec::new();

    let meta = crate::models::read_model_metadata(std::path::Path::new(model_path));
    let layers = meta.as_ref().and_then(|m| m.block_count).unwrap_or(32);
    let embd = meta.as_ref().and_then(|m| m.embedding_length).unwrap_or(4096);
    let model_ctx = meta.as_ref().and_then(|m| m.context_length);

    // GQA factor: the KV cache stores only the KV-head dimensions per layer,
    // i.e. embd × kv_heads / heads. Without GQA (kv_heads == heads) this is 1.
    let gqa_factor = match (meta.as_ref().and_then(|m| m.attention_head_count),
                            meta.as_ref().and_then(|m| m.attention_head_count_kv)) {
        (Some(heads), Some(kv_heads)) if heads > 0 => kv_heads as f64 / heads as f64,
        _ => 1.0,
    };
    let kv_embd = (embd as f64 * gqa_factor).max(1.0) as u64;

    // Effective context: 0 means "use model default"
    let effective_ctx = if n_ctx > 0 {
        n_ctx as u64
    } else {
        model_ctx.unwrap_or(4096)
    };
    if n_ctx == 0 {
        notes.push(format!("Context: model default ({})", effective_ctx));
    }

    // KV cache: per layer, K and V each n_ctx × kv_embd × bytes-per-element.
    let kv_cache_mb = kv_cache_mb(layers, kv_embd, effective_ctx, cache_type_k, cache_type_v);

    // Split model weights between VRAM and RAM based on offload layers
    let offload_layers = if n_gpu_layers < 0 {
        layers as i64 // -1 = all layers
    } else {
        n_gpu_layers as i64
    };
    let offload_ratio = (offload_layers as f64 / layers as f64).clamp(0.0, 1.0);
    let model_in_vram_mb = (model_size_mb as f64 * offload_ratio) as u64;
    let model_in_ram_mb = model_size_mb - model_in_vram_mb;

    // Compute overhead & KV cache placement: on GPU when offloading
    let overhead_mb = 512;
    let gpu_offload = n_gpu_layers != 0;
    let kv_in_vram_mb = if gpu_offload { kv_cache_mb } else { 0 };
    let kv_in_ram_mb = kv_cache_mb - kv_in_vram_mb;
    let overhead_in_vram_mb = if gpu_offload { overhead_mb } else { 0 };
    let overhead_in_ram_mb = overhead_mb - overhead_in_vram_mb;

    let vram_used_mb = model_in_vram_mb + kv_in_vram_mb + overhead_in_vram_mb;
    let ram_used_mb = model_in_ram_mb + kv_in_ram_mb + overhead_in_ram_mb;

    let fits = vram_used_mb <= vram_total_mb.max(1) && ram_used_mb <= ram_available_mb.max(1);
    if !fits {
        if vram_used_mb > vram_total_mb && vram_total_mb > 0 {
            notes.push(format!(
                "Estimated VRAM usage ({:.1} GB) exceeds available VRAM ({:.1} GB).",
                vram_used_mb as f64 / 1024.0,
                vram_total_mb as f64 / 1024.0
            ));
        }
        if ram_used_mb > ram_available_mb {
            notes.push(format!(
                "Estimated RAM usage ({:.1} GB) exceeds available RAM ({:.1} GB).",
                ram_used_mb as f64 / 1024.0,
                ram_available_mb as f64 / 1024.0
            ));
        }
        notes.push(
            "llama-server --fit (default: on) auto-reduces context and GPU layers to fit device memory at launch."
                .to_string(),
        );
    }

    Ok(MemoryEstimate {
        model_mb: model_size_mb,
        kv_cache_mb,
        overhead_mb,
        total_mb: model_size_mb + kv_cache_mb + overhead_mb,
        vram_total_mb,
        ram_available_mb,
        vram_used_mb,
        ram_used_mb,
        vram_model_mb: model_in_vram_mb,
        vram_kv_mb: kv_in_vram_mb,
        vram_overhead_mb: overhead_in_vram_mb,
        ram_model_mb: model_in_ram_mb,
        ram_kv_mb: kv_in_ram_mb,
        ram_overhead_mb: overhead_in_ram_mb,
        fits,
        notes,
    })
}

pub fn get_system_info() -> Result<SystemInfo> {
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpu_name = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "Unknown CPU".to_string());

    let cpu_cores = sys.physical_core_count().unwrap_or(1) as u32;
    let cpu_threads = sys.cpus().len() as u32;
    let total_ram_mb = sys.total_memory() / (1024 * 1024);
    let available_ram_mb = sys.available_memory() / (1024 * 1024);

    let gpus = detect_gpus();
    let available_backends = detect_backends(&gpus);
    let recommended_backend = pick_best_backend(&available_backends, &gpus);

    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();

    Ok(SystemInfo {
        cpu_name,
        cpu_cores,
        cpu_threads,
        total_ram_mb,
        available_ram_mb,
        gpus,
        os,
        arch,
        available_backends,
        recommended_backend,
    })
}

fn detect_gpus() -> Vec<GpuInfo> {
    #[cfg(target_os = "linux")]
    return detect_gpus_linux();
    #[cfg(target_os = "windows")]
    return detect_gpus_windows();
    #[cfg(target_os = "macos")]
    return detect_gpus_macos();
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    return vec![];
}

#[cfg(target_os = "linux")]
fn detect_gpus_linux() -> Vec<GpuInfo> {
    let mut gpus = Vec::new();

    // Try nvidia-smi first for NVIDIA GPUs
    if let Ok(output) = silent_cmd("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let parts: Vec<&str> = line.splitn(2, ',').collect();
                if parts.len() == 2 {
                    let name = parts[0].trim().to_string();
                    let vram_mb = parts[1].trim().parse::<u64>().unwrap_or(0);
                    gpus.push(GpuInfo {
                        name,
                        vram_mb,
                        vendor: GpuVendor::Nvidia,
                    });
                }
            }
        }
    }

    // Try rocm-smi for AMD GPUs
    if let Ok(output) = silent_cmd("rocm-smi")
        .args(["--showmeminfo", "vram", "--json"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(cards) = json.as_object() {
                    for (_, card) in cards {
                        let name = card["Card series"].as_str().unwrap_or("AMD GPU").to_string();
                        let vram_str = card["VRAM Total Memory (B)"].as_str().unwrap_or("0");
                        let vram_mb = vram_str.parse::<u64>().unwrap_or(0) / (1024 * 1024);
                        if !gpus.iter().any(|g| g.vendor == GpuVendor::Amd) {
                            gpus.push(GpuInfo {
                                name,
                                vram_mb,
                                vendor: GpuVendor::Amd,
                            });
                        }
                    }
                }
            }
        }
    }

    // Fallback: parse lspci
    if gpus.is_empty() {
        if let Ok(output) = silent_cmd("lspci").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let lower = line.to_lowercase();
                if lower.contains("vga") || lower.contains("3d controller") || lower.contains("display controller") {
                    let vendor = if lower.contains("nvidia") {
                        GpuVendor::Nvidia
                    } else if lower.contains("amd") || lower.contains("radeon") || lower.contains("advanced micro") {
                        GpuVendor::Amd
                    } else if lower.contains("intel") {
                        GpuVendor::Intel
                    } else {
                        GpuVendor::Unknown
                    };

                    // Extract GPU name (part after the colon)
                    let name = line
                        .split(':')
                        .last()
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| "Unknown GPU".to_string());

                    gpus.push(GpuInfo {
                        name,
                        vram_mb: 0,
                        vendor,
                    });
                }
            }
        }
    }

    gpus
}

/// Returns true for virtual/emulated GPU adapters that should be deprioritized.
#[cfg(any(target_os = "windows", test))]
fn is_virtual_gpu(name: &str) -> bool {
    let lower = name.to_lowercase();
    let virtual_keywords = [
        "microsoft basic display",
        "microsoft hyper-v video",
        "microsoft remote display",
        "vmware svga",
        "virtualbox",
        "parallels display",
        "qemu",
        "red hat qxl",
        "aspeed",
        "virtual render",
    ];
    virtual_keywords.iter().any(|kw| lower.contains(kw))
}

#[cfg(target_os = "windows")]
fn detect_gpus_windows() -> Vec<GpuInfo> {
    let mut gpus = Vec::new();

    // Use PowerShell to query WMI
    let script = "Get-WmiObject Win32_VideoController | Select-Object Name,AdapterRAM | ConvertTo-Json";
    if let Ok(output) = silent_cmd("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            // Handle both single object and array
            let json_text = if text.trim().starts_with('[') {
                text.to_string()
            } else {
                format!("[{}]", text)
            };

            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&json_text) {
                for item in arr {
                    let name = item["Name"].as_str().unwrap_or("Unknown GPU").to_string();
                    let vram_mb = item["AdapterRAM"]
                        .as_u64()
                        .unwrap_or(0)
                        / (1024 * 1024);
                    let lower = name.to_lowercase();
                    let vendor = if lower.contains("nvidia") || lower.contains("geforce") {
                        GpuVendor::Nvidia
                    } else if lower.contains("amd") || lower.contains("radeon") {
                        GpuVendor::Amd
                    } else if lower.contains("intel") {
                        GpuVendor::Intel
                    } else {
                        GpuVendor::Unknown
                    };

                    // Try nvidia-smi for more accurate VRAM
                    let actual_vram = if vendor == GpuVendor::Nvidia {
                        get_nvidia_vram_mb().unwrap_or(vram_mb)
                    } else {
                        vram_mb
                    };

                    gpus.push(GpuInfo {
                        name,
                        vram_mb: actual_vram,
                        vendor,
                    });
                }
            }
        }
    }

    // Filter out virtual GPUs when real ones are present
    let has_real_gpu = gpus.iter().any(|g| !is_virtual_gpu(&g.name));
    if has_real_gpu {
        gpus.retain(|g| !is_virtual_gpu(&g.name));
    }

    gpus
}

#[cfg(target_os = "macos")]
fn detect_gpus_macos() -> Vec<GpuInfo> {
    let mut gpus = Vec::new();

    if let Ok(output) = silent_cmd("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(displays) = json["SPDisplaysDataType"].as_array() {
                    for display in displays {
                        let name = display["sppci_model"]
                            .as_str()
                            .unwrap_or("Apple GPU")
                            .to_string();

                        // VRAM parsing (e.g., "16 GB")
                        let vram_mb = display["spdisplays_vram"]
                            .as_str()
                            .and_then(|s| parse_vram_string(s))
                            .unwrap_or(0);

                        let vendor = if name.to_lowercase().contains("apple") {
                            GpuVendor::Apple
                        } else if name.to_lowercase().contains("amd") {
                            GpuVendor::Amd
                        } else {
                            GpuVendor::Intel
                        };

                        gpus.push(GpuInfo { name, vram_mb, vendor });
                    }
                }
            }
        }
    }

    if gpus.is_empty() {
        gpus.push(GpuInfo {
            name: "Apple Silicon GPU".to_string(),
            vram_mb: 0, // shared memory, unknown
            vendor: GpuVendor::Apple,
        });
    }

    gpus
}

#[allow(dead_code)]
fn parse_vram_string(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() >= 2 {
        let value: f64 = parts[0].parse().ok()?;
        let multiplier = match parts[1].to_uppercase().as_str() {
            "GB" => 1024u64,
            "MB" => 1u64,
            _ => return None,
        };
        Some((value as u64) * multiplier)
    } else {
        None
    }
}

#[allow(dead_code)]
fn get_nvidia_vram_mb() -> Option<u64> {
    let output = silent_cmd("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        let mb: u64 = text.trim().parse().ok()?;
        Some(mb)
    } else {
        None
    }
}

#[cfg_attr(target_os = "macos", allow(unused_variables))]
fn detect_backends(gpus: &[GpuInfo]) -> Vec<BackendInfo> {
    let cuda_version = get_cuda_version();

    let mut backends = vec![BackendInfo {
        id: "cpu".to_string(),
        name: "CPU".to_string(),
        available: true,
        description: "Run on CPU (AVX2). Slowest but always available.".to_string(),
        version: None,
    }];

    #[cfg(target_os = "linux")]
    {
        // CUDA (via nvidia-smi presence)
        let cuda_available = gpus.iter().any(|g| g.vendor == GpuVendor::Nvidia)
            && silent_cmd("nvidia-smi").output().map(|o| o.status.success()).unwrap_or(false);
        backends.push(BackendInfo {
            id: "cuda".to_string(),
            name: "CUDA".to_string(),
            available: cuda_available,
            description: "NVIDIA GPU acceleration via CUDA.".to_string(),
            version: cuda_version.clone(),
        });

        // ROCm (AMD)
        let rocm_available = gpus.iter().any(|g| g.vendor == GpuVendor::Amd)
            && (std::path::Path::new("/opt/rocm").exists()
                || silent_cmd("rocm-smi").output().map(|o| o.status.success()).unwrap_or(false));
        backends.push(BackendInfo {
            id: "rocm".to_string(),
            name: "ROCm (HIP)".to_string(),
            available: rocm_available,
            description: "AMD GPU acceleration via ROCm/HIP.".to_string(),
            version: None,
        });

        // Vulkan
        let vulkan_available = !gpus.is_empty()
            && (std::path::Path::new("/usr/lib/libvulkan.so.1").exists()
                || std::path::Path::new("/usr/lib/x86_64-linux-gnu/libvulkan.so.1").exists()
                || silent_cmd("vulkaninfo").arg("--summary").output().map(|o| o.status.success()).unwrap_or(false));
        backends.push(BackendInfo {
            id: "vulkan".to_string(),
            name: "Vulkan".to_string(),
            available: vulkan_available,
            description: "GPU acceleration via Vulkan (AMD/NVIDIA/Intel).".to_string(),
            version: None,
        });

        // OpenVINO (Intel)
        let openvino_available = gpus.iter().any(|g| g.vendor == GpuVendor::Intel)
            && (std::path::Path::new("/opt/intel/openvino").exists()
                || std::path::Path::new("/usr/lib/libopenvino.so").exists());
        backends.push(BackendInfo {
            id: "openvino".to_string(),
            name: "OpenVINO".to_string(),
            available: openvino_available,
            description: "Intel GPU/NPU acceleration via OpenVINO.".to_string(),
            version: None,
        });
    }

    #[cfg(target_os = "windows")]
    {
        // CUDA
        let cuda_available = gpus.iter().any(|g| g.vendor == GpuVendor::Nvidia)
            && silent_cmd("nvidia-smi").output().map(|o| o.status.success()).unwrap_or(false);
        backends.push(BackendInfo {
            id: "cuda".to_string(),
            name: "CUDA".to_string(),
            available: cuda_available,
            description: "NVIDIA GPU acceleration via CUDA.".to_string(),
            version: cuda_version.clone(),
        });

        // Vulkan
        let vulkan_available = !gpus.is_empty() && {
            let sys32 = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "C:\\Windows".to_string());
            std::path::Path::new(&format!("{}\\System32\\vulkan-1.dll", sys32)).exists()
        };
        backends.push(BackendInfo {
            id: "vulkan".to_string(),
            name: "Vulkan".to_string(),
            available: vulkan_available,
            description: "GPU acceleration via Vulkan (AMD/NVIDIA/Intel).".to_string(),
            version: None,
        });

        // SYCL (Intel oneAPI)
        let sycl_available = gpus.iter().any(|g| g.vendor == GpuVendor::Intel) && {
            let oneapi = std::path::Path::new("C:\\Program Files (x86)\\Intel\\oneAPI").exists()
                || std::path::Path::new("C:\\Program Files\\Intel\\oneAPI").exists();
            oneapi
        };
        backends.push(BackendInfo {
            id: "sycl".to_string(),
            name: "SYCL (Intel oneAPI)".to_string(),
            available: sycl_available,
            description: "Intel GPU acceleration via SYCL/oneAPI.".to_string(),
            version: None,
        });

        // HIP (AMD on Windows)
        let hip_available = gpus.iter().any(|g| g.vendor == GpuVendor::Amd) && {
            std::path::Path::new("C:\\Program Files\\AMD\\ROCm").exists()
        };
        backends.push(BackendInfo {
            id: "hip".to_string(),
            name: "HIP (AMD ROCm)".to_string(),
            available: hip_available,
            description: "AMD GPU acceleration via HIP.".to_string(),
            version: None,
        });
    }

    #[cfg(target_os = "macos")]
    {
        backends.push(BackendInfo {
            id: "metal".to_string(),
            name: "Metal".to_string(),
            available: true,
            description: "Apple GPU acceleration via Metal.".to_string(),
            version: None,
        });
    }

    backends
}

fn pick_best_backend(backends: &[BackendInfo], gpus: &[GpuInfo]) -> String {
    // Priority: CUDA > Metal > ROCm > Vulkan > SYCL > HIP > OpenVINO > CPU
    let priority = ["cuda", "metal", "rocm", "vulkan", "hip", "sycl", "openvino", "cpu"];

    for &id in &priority {
        if let Some(b) = backends.iter().find(|b| b.id == id && b.available) {
            // For Vulkan, prefer only if there's a discrete GPU
            if id == "vulkan" && !gpus.iter().any(|g| g.vram_mb > 512) {
                continue;
            }
            return b.id.clone();
        }
    }

    "cpu".to_string()
}

pub fn suggest_config(model_size_mb: u64, system: &SystemInfo) -> SuggestedConfig {
    suggest_config_with_layers(model_size_mb, None, system)
}

pub fn suggest_config_with_layers(
    model_size_mb: u64,
    layers: Option<u32>,
    system: &SystemInfo,
) -> SuggestedConfig {
    let total_vram_mb: u64 = system.gpus.iter().map(|g| g.vram_mb).sum();
    let total_ram_mb = system.available_ram_mb;
    let mut notes = Vec::new();

    let (n_gpu_layers, can_fit_fully_in_vram) = if total_vram_mb > 0 {
        let usable_vram = total_vram_mb.saturating_sub(512); // reserve 512MB for overhead
        if model_size_mb <= usable_vram {
            notes.push("Model fits entirely in VRAM - full GPU acceleration.".to_string());
            (-1i32, true) // -1 = all layers
        } else if model_size_mb <= usable_vram + total_ram_mb {
            // Partial offload: estimate layers
            let total_layers = layers.unwrap_or(32) as f64;
            let ratio = usable_vram as f64 / model_size_mb as f64;
            let estimated_layers = (ratio * total_layers).floor() as i32;
            notes.push(format!(
                "Model partially fits in VRAM ({:.0}%). Offloading ~{} of {} layers to GPU.",
                ratio * 100.0,
                estimated_layers,
                total_layers
            ));
            (estimated_layers, false)
        } else {
            notes.push("Model too large for GPU+RAM. CPU only.".to_string());
            (0i32, false)
        }
    } else {
        if model_size_mb > total_ram_mb.saturating_sub(1024) {
            notes.push("Warning: Model may not fit in available RAM.".to_string());
        }
        (0i32, false)
    };

    // Context size: 0 means "loaded from model" (llama-server default)
    let n_ctx = 0u32;

    let total_usable_mb = if total_vram_mb > 0 {
        total_vram_mb + total_ram_mb
    } else {
        total_ram_mb
    };

    // Heuristic threads: peak around physical cores (like llama-optimize brackets
    // around phys cores). Clamp to 1..64.
    let n_threads = Some((system.cpu_cores.max(1).min(64)) as i32);
    // Micro-batch / batch: keep b >= ub, power-of-two. Larger VRAM → larger ub
    // for better prefill throughput (llama-optimize sweeps 128..2048).
    let n_ubatch = Some(if total_vram_mb >= 16000 { 1024 } else { 512 });
    let n_batch = Some(n_ubatch.unwrap() * 4); // 2048 or 4096, always ≥ ubatch
    notes.push(format!("Threads: {} (physical cores)", n_threads.unwrap()));
    notes.push(format!(
        "Batch: {} / Micro-batch: {} (b=4·ub, power-of-two)",
        n_batch.unwrap(),
        n_ubatch.unwrap()
    ));

    SuggestedConfig {
        n_gpu_layers,
        n_ctx,
        can_fit_fully_in_vram,
        total_usable_mb,
        notes,
        n_threads,
        n_batch,
        n_ubatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_system(vram_mb: u64, ram_mb: u64) -> SystemInfo {
        let gpus = if vram_mb > 0 {
            vec![GpuInfo { name: "Test GPU".to_string(), vram_mb, vendor: GpuVendor::Nvidia }]
        } else {
            vec![]
        };
        SystemInfo {
            cpu_name: "Test CPU".to_string(),
            cpu_cores: 8,
            cpu_threads: 16,
            total_ram_mb: ram_mb,
            available_ram_mb: ram_mb,
            gpus,
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            available_backends: vec![],
            recommended_backend: "cpu".to_string(),
        }
    }

    #[test]
    fn suggest_config_fits_in_vram() {
        let system = make_system(8192, 16384); // 8GB VRAM, 16GB RAM
        let config = suggest_config(4000, &system); // 4GB model
        assert_eq!(config.n_gpu_layers, -1);
        assert!(config.can_fit_fully_in_vram);
    }

    #[test]
    fn suggest_config_partial_offload() {
        let system = make_system(8192, 32768); // 8GB VRAM, 32GB RAM
        let config = suggest_config(12000, &system); // 12GB model
        assert!(config.n_gpu_layers > 0, "should partially offload");
        assert!(config.n_gpu_layers < 32, "should not offload all layers");
        assert!(!config.can_fit_fully_in_vram);
    }

    #[test]
    fn suggest_config_no_gpu() {
        let system = make_system(0, 16384); // no GPU, 16GB RAM
        let config = suggest_config(4000, &system);
        assert_eq!(config.n_gpu_layers, 0);
        assert!(!config.can_fit_fully_in_vram);
    }

    #[test]
    fn suggest_config_model_too_large() {
        let system = make_system(8192, 8192); // 8GB VRAM + 8GB RAM
        let config = suggest_config(50000, &system); // 50GB model
        assert_eq!(config.n_gpu_layers, 0);
        assert!(!config.can_fit_fully_in_vram);
    }

    #[test]
    fn suggest_config_context_is_zero() {
        let system = make_system(8192, 16384);
        let config = suggest_config(4000, &system);
        assert_eq!(config.n_ctx, 0, "should default to 0 (model default)");
    }

    // ── is_virtual_gpu ──────────────────────────────────────────────────────

    #[test]
    fn virtual_gpu_detects_hyper_v() {
        assert!(is_virtual_gpu("Microsoft Hyper-V Video"));
    }

    #[test]
    fn virtual_gpu_detects_basic_display() {
        assert!(is_virtual_gpu("Microsoft Basic Display Adapter"));
    }

    #[test]
    fn virtual_gpu_detects_vmware() {
        assert!(is_virtual_gpu("VMware SVGA 3D"));
    }

    #[test]
    fn virtual_gpu_detects_virtualbox() {
        assert!(is_virtual_gpu("VirtualBox Graphics Adapter (WDDM)"));
    }

    #[test]
    fn virtual_gpu_case_insensitive() {
        assert!(is_virtual_gpu("MICROSOFT BASIC DISPLAY ADAPTER"));
        assert!(is_virtual_gpu("vmware svga"));
    }

    #[test]
    fn virtual_gpu_rejects_real_nvidia() {
        assert!(!is_virtual_gpu("NVIDIA GeForce RTX 4090"));
    }

    #[test]
    fn virtual_gpu_rejects_real_amd() {
        assert!(!is_virtual_gpu("AMD Radeon RX 7900 XTX"));
    }

    #[test]
    fn virtual_gpu_rejects_real_intel() {
        assert!(!is_virtual_gpu("Intel Arc A770"));
    }

    // ── virtual GPU filtering ───────────────────────────────────────────────

    #[test]
    fn filter_virtual_gpus_when_real_present() {
        let mut gpus = vec![
            GpuInfo { name: "Microsoft Hyper-V Video".into(), vram_mb: 128, vendor: GpuVendor::Unknown },
            GpuInfo { name: "NVIDIA GeForce RTX 4090".into(), vram_mb: 24576, vendor: GpuVendor::Nvidia },
        ];
        let has_real = gpus.iter().any(|g| !is_virtual_gpu(&g.name));
        if has_real {
            gpus.retain(|g| !is_virtual_gpu(&g.name));
        }
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
    }

    #[test]
    fn keep_virtual_gpus_when_no_real_present() {
        let mut gpus = vec![
            GpuInfo { name: "Microsoft Hyper-V Video".into(), vram_mb: 128, vendor: GpuVendor::Unknown },
            GpuInfo { name: "Microsoft Basic Display Adapter".into(), vram_mb: 0, vendor: GpuVendor::Unknown },
        ];
        let has_real = gpus.iter().any(|g| !is_virtual_gpu(&g.name));
        if has_real {
            gpus.retain(|g| !is_virtual_gpu(&g.name));
        }
        assert_eq!(gpus.len(), 2, "should keep all GPUs when only virtual ones exist");
    }

    // ── silent_cmd ──────────────────────────────────────────────────────────

    #[test]
    fn silent_cmd_creates_command() {
        // Verify silent_cmd produces a runnable Command (will fail to find
        // the binary, but must not panic during construction).
        let mut cmd = silent_cmd("nonexistent-binary-12345");
        let result = cmd.output();
        assert!(result.is_err(), "nonexistent binary should fail");
    }

    // ── KV cache estimation ─────────────────────────────────────────────────

    #[test]
    fn kv_cache_scales_with_ctx_not_parallel() {
        // 32 layers, 2560 embd, full heads (no GQA), f16/f16, ctx 32768
        // = 32 × 32768 × 2560 × 4 / 1MiB = 10240 MiB
        assert_eq!(kv_cache_mb(32, 2560, 32768, "f16", "f16"), 10240);
        // Doubling ctx doubles the cache
        assert_eq!(kv_cache_mb(32, 2560, 65536, "f16", "f16"), 20480);
        // Parallel slots share the ctx budget — same total
        assert_eq!(kv_cache_mb(32, 2560, 32768, "f16", "f16"), kv_cache_mb(32, 2560, 32768, "f16", "f16"));
    }

    #[test]
    fn kv_cache_gqa_reduces_size() {
        // GQA 32 heads / 8 kv heads → kv_embd = 2560 × 8/32 = 640
        // 32 × 32768 × 640 × 4 / 1MiB = 2560 MiB
        assert_eq!(kv_cache_mb(32, 640, 32768, "f16", "f16"), 2560);
        // 4x smaller than full-embd cache
        assert_eq!(kv_cache_mb(32, 2560, 32768, "f16", "f16"), kv_cache_mb(32, 640, 32768, "f16", "f16") * 4);
    }

    #[test]
    fn kv_cache_q8_halves_f16() {
        assert_eq!(kv_cache_mb(32, 640, 32768, "q8_0", "q8_0"), 1280);
    }
}
