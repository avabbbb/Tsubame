//! GPU discovery and the local ASR tier it implies (ADR pending, M3a).
//!
//! Tiers, from public benchmarks (docs/16-modular-asr.md):
//! - Qwen3-ASR-1.7B + Qwen3-ForcedAligner-0.6B needs about 4 GB for the ASR
//!   model alone, so it is planned only on NVIDIA Turing or newer with at
//!   least 8 GB, or on an AMD RDNA3/RDNA4 card that ROCm PyTorch supports on
//!   Windows.
//! - Everything else runs faster-whisper large-v3-turbo: CUDA on any other
//!   NVIDIA card (the worker picks int8 on Pascal), CPU otherwise.

use serde::Serialize;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub name: String,
    /// Dedicated memory in MiB, when the source reports it reliably.
    pub vram_mib: Option<u64>,
    /// NVIDIA compute capability (major, minor).
    pub compute_capability: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AsrTier {
    Qwen3Cuda,
    Qwen3Rocm,
    FasterWhisperCuda,
    FasterWhisperCpu,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TierChoice {
    pub tier: AsrTier,
    pub gpu: Option<String>,
    pub reason: String,
}

pub const QWEN3_MIN_VRAM_MIB: u64 = 8000;
/// Turing (sm_75) is the oldest NVIDIA generation with fast FP16 tensor cores
/// that current CUDA PyTorch wheels still target.
pub const QWEN3_MIN_COMPUTE: (u32, u32) = (7, 5);

/// AMD cards listed for PyTorch on Windows in AMD's ROCm compatibility matrix
/// (RDNA4 gfx1200/gfx1201, RDNA3 gfx1100/gfx1101). Matched on the adapter name.
const ROCM_WINDOWS_CARDS: &[&str] = &[
    "RX 9070",
    "RX 9060",
    "AI PRO R9700",
    "RX 7900",
    "RX 7800",
    "RX 7700",
    "PRO W7900",
    "PRO W7800",
    "PRO W7700",
];

pub fn amd_rocm_supported(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ROCM_WINDOWS_CARDS.iter().any(|card| upper.contains(card))
}

/// Pure tier policy; gpus in any order.
pub fn choose_asr_tier(gpus: &[GpuInfo]) -> TierChoice {
    let qwen_nvidia = gpus.iter().find(|gpu| {
        gpu.vendor == GpuVendor::Nvidia
            && gpu
                .compute_capability
                .is_some_and(|cc| cc >= QWEN3_MIN_COMPUTE)
            && gpu.vram_mib.is_some_and(|mib| mib >= QWEN3_MIN_VRAM_MIB)
    });
    if let Some(gpu) = qwen_nvidia {
        return TierChoice {
            tier: AsrTier::Qwen3Cuda,
            gpu: Some(gpu.name.clone()),
            reason: format!(
                "{} has Turing+ and at least 8 GB: Qwen3-ASR on CUDA",
                gpu.name
            ),
        };
    }
    if let Some(gpu) = gpus
        .iter()
        .find(|gpu| gpu.vendor == GpuVendor::Amd && amd_rocm_supported(&gpu.name))
    {
        return TierChoice {
            tier: AsrTier::Qwen3Rocm,
            gpu: Some(gpu.name.clone()),
            reason: format!(
                "{} is supported by ROCm PyTorch on Windows: Qwen3-ASR on ROCm",
                gpu.name
            ),
        };
    }
    if let Some(gpu) = gpus.iter().find(|gpu| gpu.vendor == GpuVendor::Nvidia) {
        return TierChoice {
            tier: AsrTier::FasterWhisperCuda,
            gpu: Some(gpu.name.clone()),
            reason: format!(
                "{} is below the Qwen3 tier (needs Turing+ and 8 GB): faster-whisper on CUDA",
                gpu.name
            ),
        };
    }
    TierChoice {
        tier: AsrTier::FasterWhisperCpu,
        gpu: None,
        reason: "no supported GPU: faster-whisper on CPU".into(),
    }
}

/// Parses nvidia-smi --query-gpu=name,memory.total,compute_cap
/// --format=csv,noheader,nounits output.
pub fn parse_nvidia_smi(output: &str) -> Vec<GpuInfo> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            if fields.len() < 3 || fields[0].is_empty() {
                return None;
            }
            let compute_capability = fields[2]
                .split_once('.')
                .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)));
            Some(GpuInfo {
                vendor: GpuVendor::Nvidia,
                name: fields[0].to_string(),
                vram_mib: fields[1].parse().ok(),
                compute_capability,
            })
        })
        .collect()
}

/// Parses Get-CimInstance Win32_VideoController rows printed as
/// Name|AdapterCompatibility. Win32 AdapterRAM caps at 4 GB, so memory is
/// left unknown here; AMD tiers go by the supported-card list instead.
pub fn parse_video_controllers(output: &str) -> Vec<GpuInfo> {
    output
        .lines()
        .filter_map(|line| {
            let (name, maker) = line.trim().split_once('|')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let maker = maker.to_ascii_lowercase();
            let vendor = if maker.contains("nvidia") {
                GpuVendor::Nvidia
            } else if maker.contains("advanced micro devices") || maker.contains("amd") {
                GpuVendor::Amd
            } else {
                GpuVendor::Other
            };
            Some(GpuInfo {
                vendor,
                name: name.to_string(),
                vram_mib: None,
                compute_capability: None,
            })
        })
        .collect()
}

fn run_hidden(program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// NVIDIA cards from nvidia-smi (exact memory and compute capability), plus
/// every non-NVIDIA adapter Windows reports.
pub fn detect_gpus() -> Vec<GpuInfo> {
    let mut gpus = run_hidden(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,compute_cap",
            "--format=csv,noheader,nounits",
        ],
    )
    .map(|out| parse_nvidia_smi(&out))
    .unwrap_or_default();
    if cfg!(windows) {
        let others = run_hidden(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name + '|' + $_.AdapterCompatibility }",
            ],
        )
        .map(|out| parse_video_controllers(&out))
        .unwrap_or_default();
        gpus.extend(
            others
                .into_iter()
                .filter(|gpu| gpu.vendor != GpuVendor::Nvidia),
        );
    }
    gpus
}

pub fn detect_asr_tier() -> TierChoice {
    choose_asr_tier(&detect_gpus())
}

#[cfg(test)]
mod tests {
    use super::{
        choose_asr_tier, parse_nvidia_smi, parse_video_controllers, AsrTier, GpuInfo, GpuVendor,
    };

    fn nvidia(name: &str, vram: u64, cc: (u32, u32)) -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Nvidia,
            name: name.into(),
            vram_mib: Some(vram),
            compute_capability: Some(cc),
        }
    }

    fn amd(name: &str) -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Amd,
            name: name.into(),
            vram_mib: None,
            compute_capability: None,
        }
    }

    #[test]
    fn laptop_3050_ti_stays_on_faster_whisper_cuda() {
        let choice = choose_asr_tier(&[nvidia(
            "NVIDIA GeForce RTX 3050 Ti Laptop GPU",
            4096,
            (8, 6),
        )]);
        assert_eq!(choice.tier, AsrTier::FasterWhisperCuda);
    }

    #[test]
    fn gtx_1070_is_pascal_so_faster_whisper_cuda() {
        let choice = choose_asr_tier(&[nvidia("NVIDIA GeForce GTX 1070", 8192, (6, 1))]);
        assert_eq!(choice.tier, AsrTier::FasterWhisperCuda);
    }

    #[test]
    fn rtx_3060_12gb_gets_qwen3_cuda() {
        let choice = choose_asr_tier(&[nvidia("NVIDIA GeForce RTX 3060", 12288, (8, 6))]);
        assert_eq!(choice.tier, AsrTier::Qwen3Cuda);
    }

    #[test]
    fn rx_9070_gre_gets_qwen3_rocm() {
        let choice = choose_asr_tier(&[amd("AMD Radeon RX 9070 GRE")]);
        assert_eq!(choice.tier, AsrTier::Qwen3Rocm);
        assert_eq!(choice.gpu.as_deref(), Some("AMD Radeon RX 9070 GRE"));
    }

    #[test]
    fn unsupported_amd_and_igpu_fall_back_to_cpu() {
        let choice = choose_asr_tier(&[amd("AMD Radeon RX 6600"), amd("AMD Radeon(TM) Graphics")]);
        assert_eq!(choice.tier, AsrTier::FasterWhisperCpu);
    }

    #[test]
    fn qwen3_capable_card_wins_over_a_weaker_one() {
        let choice = choose_asr_tier(&[
            nvidia("NVIDIA GeForce GTX 1070", 8192, (6, 1)),
            amd("AMD Radeon RX 9070 GRE"),
        ]);
        assert_eq!(choice.tier, AsrTier::Qwen3Rocm);
    }

    #[test]
    fn no_gpu_is_cpu() {
        assert_eq!(choose_asr_tier(&[]).tier, AsrTier::FasterWhisperCpu);
    }

    #[test]
    fn parses_nvidia_smi_rows() {
        let gpus = parse_nvidia_smi("NVIDIA GeForce RTX 3050 Ti Laptop GPU, 4096, 8.6\nNVIDIA GeForce GTX 1070, 8192, 6.1\n");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].vram_mib, Some(4096));
        assert_eq!(gpus[1].compute_capability, Some((6, 1)));
    }

    #[test]
    fn parses_video_controller_rows() {
        let gpus = parse_video_controllers(
            "AMD Radeon RX 9070 GRE|Advanced Micro Devices, Inc.\nIntel(R) UHD Graphics|Intel Corporation\n",
        );
        assert_eq!(gpus[0].vendor, GpuVendor::Amd);
        assert_eq!(gpus[1].vendor, GpuVendor::Other);
    }
}
