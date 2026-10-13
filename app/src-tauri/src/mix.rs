//! `audio.mix`: offline render of `original + Σ placed dub clips`, and a dub-only stem.
//!
//! Contract: `docs/19-dubbing-pipeline.md` § audio.mix.
//!
//! Invariants:
//! - The original is never altered. Every frame where no dub clip contributes is
//!   written from the original's raw samples unchanged (bit-identical), with or
//!   without the limiter. Where a clip contributes, original and dub are summed.
//! - The optional limiter acts on the sum bus only (never on the original or the
//!   dub alone, never on the stem), only where a dub clip contributes (plus its
//!   release tail), and never pulls the sum below the original's own peak.
//! - Dub clips are never sped up, stretched or truncated. Resampling preserves
//!   duration; when the dub runs past the end of the original, the output is
//!   extended with silence on the original side.
//!
//! Loudness match (`active_rms_dbfs`, target = source active RMS + offset, clamped,
//! one gain capped by a max boost, short edge fades, per-clip peak ceiling) and the
//! instant-attack / smooth-release peak ceiling are ported from ASMR-Dubber
//! `audio.py` (`active_rms_dbfs`, `prepare_chinese_clip`, `build_chinese_stem`,
//! `_sum_with_peak_ceiling`).
//! Copyright (c) EveningStudy and ASMR-Dubber contributors, MIT License,
//! https://github.com/EveningStudy/asmr-dubber (commit `2faea94f`).

use crate::placement::{self, Placement, PlacementParams};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

pub const ERR_CANCELLED: &str = "cancelled";
const PROGRESS_EVERY_FRAMES: u64 = 65_536;
/// Analysis frame for the active-RMS gate (Dubber default).
const ACTIVE_RMS_FRAME_MS: f64 = 50.0;
/// Tolerance between the duration a placement assumed and the decoded clip.
const CLIP_DURATION_TOLERANCE_MS: i64 = 50;

/// Progress/cancellation hook. `jobs.rs` (another track) adapts its job handle to this.
pub trait ProgressSink: Send {
    fn progress(&self, stage: &str, done: u64, total: u64);
    fn is_cancelled(&self) -> bool;
}

pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn progress(&self, _stage: &str, _done: u64, _total: u64) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind {
    /// Original + dub, same WAV format as the original.
    #[default]
    Mix,
    /// Dub clips only, 32-bit float WAV at the original rate/channel count.
    DubStem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixOptions {
    pub kind: ExportKind,
    pub limiter: bool,
    pub limiter_ceiling_dbfs: f64,
    pub limiter_release_ms: f64,
    /// User gain on the dub layer after loudness matching.
    pub dub_gain_db: f64,
    pub loudness_match: bool,
    pub relative_loudness_db: f64,
    pub min_active_rms_dbfs: f64,
    pub max_active_rms_dbfs: f64,
    pub max_boost_db: f64,
    pub clip_peak_dbfs: Option<f64>,
    pub fade_ms: f64,
    pub placement: PlacementParams,
}

impl Default for MixOptions {
    fn default() -> Self {
        Self {
            kind: ExportKind::Mix,
            limiter: true,
            limiter_ceiling_dbfs: -1.0,
            limiter_release_ms: 50.0,
            dub_gain_db: 0.0,
            loudness_match: true,
            relative_loudness_db: -8.0,
            min_active_rms_dbfs: -42.0,
            max_active_rms_dbfs: -30.0,
            max_boost_db: 12.0,
            clip_peak_dbfs: Some(-9.0),
            fade_ms: 8.0,
            placement: PlacementParams::default(),
        }
    }
}

impl MixOptions {
    pub fn validate(&self) -> Result<(), String> {
        let finite = [
            self.limiter_ceiling_dbfs,
            self.limiter_release_ms,
            self.dub_gain_db,
            self.relative_loudness_db,
            self.min_active_rms_dbfs,
            self.max_active_rms_dbfs,
            self.max_boost_db,
            self.fade_ms,
        ];
        if finite.iter().any(|value| !value.is_finite())
            || matches!(self.clip_peak_dbfs, Some(v) if !v.is_finite())
        {
            return Err("mix options must be finite numbers".to_string());
        }
        if !(-12.0..0.0).contains(&self.limiter_ceiling_dbfs) {
            return Err("limiter_ceiling_dbfs must be within -12..0 dBFS".to_string());
        }
        if !(1.0..=1000.0).contains(&self.limiter_release_ms) {
            return Err("limiter_release_ms must be within 1..1000 ms".to_string());
        }
        if !(-60.0..=24.0).contains(&self.dub_gain_db) {
            return Err("dub_gain_db must be within -60..24 dB".to_string());
        }
        if self.min_active_rms_dbfs > self.max_active_rms_dbfs {
            return Err("min_active_rms_dbfs must not exceed max_active_rms_dbfs".to_string());
        }
        if !(0.0..=40.0).contains(&self.max_boost_db) {
            return Err("max_boost_db must be within 0..40 dB".to_string());
        }
        if !(0.0..=50.0).contains(&self.fade_ms) {
            return Err("fade_ms must be within 0..50 ms".to_string());
        }
        if matches!(self.clip_peak_dbfs, Some(v) if v > 0.0) {
            return Err("clip_peak_dbfs must be <= 0 dBFS".to_string());
        }
        self.placement.validate()
    }
}

/// One placed dub clip to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixClip {
    pub segment_id: i64,
    pub path: PathBuf,
    pub dub_start_ms: i64,
    pub src_start_ms: i64,
    pub src_end_ms: i64,
    /// Duration the placement assumed; the decoded clip must match it.
    pub expected_duration_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClipReport {
    pub segment_id: i64,
    pub start_frame: u64,
    pub frames: u64,
    pub source_active_rms_dbfs: Option<f64>,
    pub clip_active_rms_dbfs: Option<f64>,
    pub target_active_rms_dbfs: Option<f64>,
    pub gain_db: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MixReport {
    pub output_path: String,
    pub kind: ExportKind,
    pub sample_rate: u32,
    pub channels: u16,
    pub original_frames: u64,
    pub output_frames: u64,
    /// Frames the limiter changed (gain < 1).
    pub limited_frames: u64,
    pub clips: Vec<ClipReport>,
}

// ---------------------------------------------------------------------------
// Sample conversion
// ---------------------------------------------------------------------------

trait Pcm: hound::Sample + Copy {
    fn to_unit(self, bits: u16) -> f64;
    fn from_unit(value: f64, bits: u16) -> Self;
    fn zero() -> Self;
}

impl Pcm for i32 {
    fn to_unit(self, bits: u16) -> f64 {
        self as f64 / (1u64 << (bits - 1)) as f64
    }
    fn from_unit(value: f64, bits: u16) -> Self {
        let scale = (1u64 << (bits - 1)) as f64;
        (value * scale).round().clamp(-scale, scale - 1.0) as i32
    }
    fn zero() -> Self {
        0
    }
}

impl Pcm for f32 {
    fn to_unit(self, _bits: u16) -> f64 {
        self as f64
    }
    fn from_unit(value: f64, _bits: u16) -> Self {
        value as f32
    }
    fn zero() -> Self {
        0.0
    }
}

fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

fn ms_to_frames(ms: i64, rate: u32) -> u64 {
    ((ms.max(0) as u128 * rate as u128 + 500) / 1000) as u64
}

fn open_wav(path: &Path) -> Result<WavReader<BufReader<fs::File>>, String> {
    WavReader::open(path).map_err(|e| {
        format!(
            "unsupported_source_format: {} could not be read as WAV ({e}); \
             this build mixes WAV only",
            path.display()
        )
    })
}

/// Reads a whole (short) WAV into per-channel unit samples.
fn read_planar<R: Read>(reader: WavReader<R>) -> Result<(WavSpec, Vec<Vec<f64>>), String> {
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut planar = vec![Vec::with_capacity(reader.duration() as usize); channels];
    match spec.sample_format {
        SampleFormat::Float => {
            for (i, sample) in reader.into_samples::<f32>().enumerate() {
                planar[i % channels].push(sample.map_err(|e| e.to_string())? as f64);
            }
        }
        SampleFormat::Int => {
            for (i, sample) in reader.into_samples::<i32>().enumerate() {
                planar[i % channels].push(
                    sample
                        .map_err(|e| e.to_string())?
                        .to_unit(spec.bits_per_sample),
                );
            }
        }
    }
    let frames = planar.iter().map(Vec::len).min().unwrap_or(0);
    planar
        .iter_mut()
        .for_each(|channel| channel.truncate(frames));
    Ok((spec, planar))
}

// ---------------------------------------------------------------------------
// Loudness (ported from Dubber `active_rms_dbfs`)
// ---------------------------------------------------------------------------

fn gated_rms_dbfs(frame_mean_squares: &[f64]) -> Option<f64> {
    let rms = frame_mean_squares
        .iter()
        .map(|ms| ms.sqrt())
        .collect::<Vec<_>>();
    let maximum = rms.iter().cloned().fold(0.0, f64::max);
    if maximum <= 1e-8 {
        return None;
    }
    // Relative 20 dB gate: ignores leading/trailing silence without assuming a
    // broadcast loudness threshold for quiet ASMR speech.
    let active = rms
        .iter()
        .filter(|value| **value >= maximum * 0.1)
        .collect::<Vec<_>>();
    let mean_square = active.iter().map(|v| *v * *v).sum::<f64>() / active.len() as f64;
    Some(20.0 * mean_square.sqrt().max(1e-12).log10())
}

/// Active RMS of a mono signal in dBFS (`None` = silence).
pub fn active_rms_dbfs(mono: &[f64], rate: u32) -> Option<f64> {
    if mono.is_empty() {
        return None;
    }
    let frame = ((rate as f64 * ACTIVE_RMS_FRAME_MS / 1000.0).round() as usize).max(1);
    let squares = mono
        .chunks(frame)
        .map(|chunk| chunk.iter().map(|v| v * v).sum::<f64>() / frame as f64)
        .collect::<Vec<_>>();
    gated_rms_dbfs(&squares)
}

/// Loudness target: source active RMS + offset, clamped to [min, max];
/// a silent source falls back to the minimum (Dubber behaviour).
pub fn loudness_target_dbfs(source: Option<f64>, options: &MixOptions) -> f64 {
    match source {
        Some(level) => (level + options.relative_loudness_db)
            .clamp(options.min_active_rms_dbfs, options.max_active_rms_dbfs),
        None => options.min_active_rms_dbfs,
    }
}

/// 50 ms mean-square grid of the original (mono mean), for source levels.
struct SourceLevels {
    frame: u64,
    mean_squares: Vec<f64>,
}

impl SourceLevels {
    fn window_dbfs(&self, start_frame: u64, end_frame: u64) -> Option<f64> {
        let first = (start_frame / self.frame) as usize;
        let last = end_frame.div_ceil(self.frame) as usize;
        let last = last.min(self.mean_squares.len());
        if first >= last {
            return None;
        }
        gated_rms_dbfs(&self.mean_squares[first..last])
    }
}

// ---------------------------------------------------------------------------
// Resampling (duration-preserving, windowed sinc) and channel mapping
// ---------------------------------------------------------------------------

/// Output length for a duration-preserving resample.
pub fn resampled_len(len: usize, from: u32, to: u32) -> usize {
    ((len as u128 * to as u128 + from as u128 / 2) / from as u128) as usize
}

/// Band-limited resampling (Blackman-windowed sinc, 16 zero crossings).
/// Changes the sample rate only: pitch and duration are preserved.
pub fn resample(input: &[f64], from: u32, to: u32) -> Vec<f64> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    const ZERO_CROSSINGS: f64 = 16.0;
    const TABLE_RES: f64 = 512.0;
    let cutoff = (to as f64 / from as f64).min(1.0);
    let half = ZERO_CROSSINGS / cutoff;
    let table_len = (half * TABLE_RES).ceil() as usize + 2;
    let table = (0..table_len)
        .map(|i| {
            let x = i as f64 / TABLE_RES;
            if x >= half {
                return 0.0;
            }
            let arg = std::f64::consts::PI * cutoff * x;
            let sinc = if arg.abs() < 1e-12 {
                1.0
            } else {
                arg.sin() / arg
            };
            let n = x / half; // 0..1 from the centre
            let window = 0.42
                + 0.5 * (std::f64::consts::PI * n).cos()
                + 0.08 * (2.0 * std::f64::consts::PI * n).cos();
            cutoff * sinc * window
        })
        .collect::<Vec<_>>();
    let kernel = |x: f64| {
        let position = x.abs() * TABLE_RES;
        let index = position as usize;
        if index + 1 >= table.len() {
            return 0.0;
        }
        let frac = position - index as f64;
        table[index] * (1.0 - frac) + table[index + 1] * frac
    };
    let out_len = resampled_len(input.len(), from, to);
    let step = from as f64 / to as f64;
    (0..out_len)
        .map(|i| {
            let t = i as f64 * step;
            let lo = ((t - half).ceil() as i64).max(0);
            let hi = ((t + half).floor() as i64).min(input.len() as i64 - 1);
            (lo..=hi)
                .map(|k| input[k as usize] * kernel(t - k as f64))
                .sum()
        })
        .collect()
}

/// Maps clip channels to the output layout without inventing spatial cues.
fn map_channels(planar: Vec<Vec<f64>>, out_channels: usize) -> Vec<Vec<f64>> {
    let frames = planar.first().map(Vec::len).unwrap_or(0);
    let in_channels = planar.len();
    if in_channels == out_channels {
        return planar;
    }
    if out_channels == 1 {
        let mut mono = vec![0.0; frames];
        for channel in &planar {
            for (acc, v) in mono.iter_mut().zip(channel) {
                *acc += v / in_channels as f64;
            }
        }
        return vec![mono];
    }
    let mut out = vec![vec![0.0; frames]; out_channels];
    if in_channels == 1 {
        if out_channels == 2 {
            out[0] = planar[0].clone();
            out[1] = planar[0].clone();
        } else {
            // Conventional 3.0/5.1/7.1 layouts: centre is the third channel
            // (Dubber `_route_mono_to_channels`).
            out[2] = planar[0].clone();
        }
        return out;
    }
    for (index, channel) in planar.into_iter().enumerate().take(out_channels) {
        out[index] = channel;
    }
    out
}

/// A clip fully prepared for summing: output rate/channels, gain applied.
struct PreparedClip {
    start_frame: u64,
    planar: Vec<Vec<f64>>,
    report: ClipReport,
}

impl PreparedClip {
    fn frames(&self) -> u64 {
        self.planar.first().map(Vec::len).unwrap_or(0) as u64
    }
}

fn prepare_clip(
    clip: &MixClip,
    out_rate: u32,
    out_channels: usize,
    source_level: Option<f64>,
    options: &MixOptions,
) -> Result<PreparedClip, String> {
    let (spec, planar) = read_planar(open_wav(&clip.path)?)?;
    let in_frames = planar.first().map(Vec::len).unwrap_or(0);
    if in_frames == 0 {
        return Err(format!("dub clip for segment {} is empty", clip.segment_id));
    }
    let decoded_ms = (in_frames as u128 * 1000 / spec.sample_rate as u128) as i64;
    if let Some(expected) = clip.expected_duration_ms {
        if (decoded_ms - expected).abs() > CLIP_DURATION_TOLERANCE_MS {
            return Err(format!(
                "clip_duration_mismatch: segment {} clip decodes to {decoded_ms} ms but was \
                 placed as {expected} ms",
                clip.segment_id
            ));
        }
    }

    let mono = (0..in_frames)
        .map(|i| planar.iter().map(|c| c[i]).sum::<f64>() / planar.len() as f64)
        .collect::<Vec<_>>();
    let clip_level = active_rms_dbfs(&mono, spec.sample_rate);
    let mut gain_db = 0.0;
    let mut target = None;
    if options.loudness_match {
        let wanted = loudness_target_dbfs(source_level, options);
        target = Some(wanted);
        if let Some(level) = clip_level {
            gain_db = (wanted - level).min(options.max_boost_db);
        }
    }

    let resampled = planar
        .iter()
        .map(|channel| resample(channel, spec.sample_rate, out_rate))
        .collect::<Vec<_>>();
    let mut mapped = map_channels(resampled, out_channels);
    let frames = mapped[0].len();

    let gain = db_to_gain(gain_db);
    let fade = ((out_rate as f64 * options.fade_ms / 1000.0).round() as usize).min(frames / 2);
    for channel in mapped.iter_mut() {
        for (i, sample) in channel.iter_mut().enumerate() {
            let mut g = gain;
            if fade > 0 {
                if i < fade {
                    g *= i as f64 / fade as f64;
                } else if i >= frames - fade {
                    g *= (frames - i) as f64 / fade as f64;
                }
            }
            *sample *= g;
        }
    }
    if let Some(peak_dbfs) = options.clip_peak_dbfs {
        let ceiling = db_to_gain(peak_dbfs);
        let peak = mapped
            .iter()
            .flat_map(|c| c.iter())
            .fold(0.0f64, |acc, v| acc.max(v.abs()));
        if peak > ceiling {
            let scale = ceiling / peak;
            mapped
                .iter_mut()
                .for_each(|c| c.iter_mut().for_each(|v| *v *= scale));
            gain_db += 20.0 * scale.log10();
        }
    }
    if options.dub_gain_db != 0.0 {
        let user = db_to_gain(options.dub_gain_db);
        mapped
            .iter_mut()
            .for_each(|c| c.iter_mut().for_each(|v| *v *= user));
        gain_db += options.dub_gain_db;
    }

    let start_frame = ms_to_frames(clip.dub_start_ms, out_rate);
    Ok(PreparedClip {
        start_frame,
        report: ClipReport {
            segment_id: clip.segment_id,
            start_frame,
            frames: frames as u64,
            source_active_rms_dbfs: source_level,
            clip_active_rms_dbfs: clip_level,
            target_active_rms_dbfs: target,
            gain_db,
        },
        planar: mapped,
    })
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn partial_path(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    output.with_file_name(name)
}

fn analyse_source<S: Pcm, R: Read>(
    reader: &mut WavReader<R>,
    sink: &dyn ProgressSink,
    total_work: u64,
) -> Result<SourceLevels, String> {
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let frame = ((spec.sample_rate as f64 * ACTIVE_RMS_FRAME_MS / 1000.0).round() as u64).max(1);
    let mut mean_squares = Vec::new();
    let mut acc = 0.0;
    let mut mono = 0.0;
    let mut in_frame = 0u64;
    let mut frames = 0u64;
    for (i, sample) in reader.samples::<S>().enumerate() {
        mono += sample
            .map_err(|e| e.to_string())?
            .to_unit(spec.bits_per_sample);
        if (i + 1) % channels != 0 {
            continue;
        }
        let value = mono / channels as f64;
        mono = 0.0;
        acc += value * value;
        in_frame += 1;
        frames += 1;
        if in_frame == frame {
            mean_squares.push(acc / frame as f64);
            acc = 0.0;
            in_frame = 0;
        }
        if frames % PROGRESS_EVERY_FRAMES == 0 {
            if sink.is_cancelled() {
                return Err(ERR_CANCELLED.to_string());
            }
            sink.progress("analyse", frames, total_work);
        }
    }
    if in_frame > 0 {
        mean_squares.push(acc / frame as f64);
    }
    Ok(SourceLevels {
        frame,
        mean_squares,
    })
}

struct Limiter {
    ceiling: f64,
    release: f64,
    gain: f64,
}

impl Limiter {
    /// Requested gain only stops the dub from pushing the sum above
    /// max(ceiling, original peak): it never limits what the original alone does.
    fn next_gain(&mut self, sum_peak: f64, original_peak: f64) -> f64 {
        let allowed = self.ceiling.max(original_peak);
        let requested = if sum_peak > allowed {
            allowed / sum_peak
        } else {
            1.0
        };
        if requested < self.gain {
            self.gain = requested; // instant attack guarantees the ceiling
        } else {
            self.gain = self.release * self.gain + (1.0 - self.release) * requested;
            if self.gain > 1.0 - 1e-6 {
                self.gain = 1.0;
            }
        }
        self.gain
    }

    fn idle(&self) -> bool {
        self.gain >= 1.0
    }
}

struct RenderCtx<'a> {
    clips: Vec<PreparedClip>,
    options: &'a MixOptions,
    sink: &'a dyn ProgressSink,
    total_work: u64,
    work_offset: u64,
}

/// Sums the dub clips active at `frame` into `dub` (zeroed first). Returns
/// whether any clip contributes. `next` is the index of the first clip not
/// started yet; clips are sorted by start.
fn dub_frame(
    clips: &[PreparedClip],
    active: &mut Vec<usize>,
    next: &mut usize,
    frame: u64,
    dub: &mut [f64],
) -> bool {
    while *next < clips.len() && clips[*next].start_frame <= frame {
        active.push(*next);
        *next += 1;
    }
    active.retain(|&i| frame < clips[i].start_frame + clips[i].frames());
    dub.iter_mut().for_each(|v| *v = 0.0);
    for &i in active.iter() {
        let offset = (frame - clips[i].start_frame) as usize;
        for (channel, value) in dub.iter_mut().enumerate() {
            *value += clips[i].planar[channel][offset];
        }
    }
    !active.is_empty()
}

fn render_mix_pass<S: Pcm, R: Read, W: Write + Seek>(
    reader: &mut WavReader<R>,
    writer: &mut WavWriter<W>,
    ctx: &RenderCtx,
    output_frames: u64,
) -> Result<u64, String> {
    let spec = reader.spec();
    let bits = spec.bits_per_sample;
    let channels = spec.channels.max(1) as usize;
    let mut samples = reader.samples::<S>();
    let mut raw = vec![S::zero(); channels];
    let mut dub = vec![0.0; channels];
    let mut active = Vec::new();
    let mut next = 0usize;
    let mut limited = 0u64;
    let mut limiter = Limiter {
        ceiling: db_to_gain(ctx.options.limiter_ceiling_dbfs),
        release: (-1.0
            / (spec.sample_rate as f64 * ctx.options.limiter_release_ms / 1000.0).max(1.0))
        .exp(),
        gain: 1.0,
    };
    for frame in 0..output_frames {
        for slot in raw.iter_mut() {
            *slot = match samples.next() {
                Some(sample) => sample.map_err(|e| e.to_string())?,
                None => S::zero(), // dub runs past the original: silence there
            };
        }
        let contributes = dub_frame(&ctx.clips, &mut active, &mut next, frame, &mut dub);
        let limiting = ctx.options.limiter && (contributes || !limiter.idle());
        if !contributes && !limiting {
            for &sample in &raw {
                writer.write_sample(sample).map_err(|e| e.to_string())?;
            }
        } else {
            let original = raw.iter().map(|s| s.to_unit(bits)).collect::<Vec<_>>();
            let sum = original
                .iter()
                .zip(&dub)
                .map(|(o, d)| o + d)
                .collect::<Vec<_>>();
            let gain = if limiting {
                let sum_peak = sum.iter().fold(0.0f64, |a, v| a.max(v.abs()));
                let original_peak = original.iter().fold(0.0f64, |a, v| a.max(v.abs()));
                limiter.next_gain(sum_peak, original_peak)
            } else {
                1.0
            };
            if gain < 1.0 {
                limited += 1;
            }
            for value in sum {
                writer
                    .write_sample(S::from_unit(value * gain, bits))
                    .map_err(|e| e.to_string())?;
            }
        }
        if (frame + 1) % PROGRESS_EVERY_FRAMES == 0 {
            if ctx.sink.is_cancelled() {
                return Err(ERR_CANCELLED.to_string());
            }
            ctx.sink
                .progress("mix", ctx.work_offset + frame + 1, ctx.total_work);
        }
    }
    Ok(limited)
}

fn render_stem_pass<W: Write + Seek>(
    writer: &mut WavWriter<W>,
    ctx: &RenderCtx,
    channels: usize,
    output_frames: u64,
) -> Result<(), String> {
    let mut dub = vec![0.0; channels];
    let mut active = Vec::new();
    let mut next = 0usize;
    for frame in 0..output_frames {
        dub_frame(&ctx.clips, &mut active, &mut next, frame, &mut dub);
        for &value in &dub {
            writer
                .write_sample(value as f32)
                .map_err(|e| e.to_string())?;
        }
        if (frame + 1) % PROGRESS_EVERY_FRAMES == 0 {
            if ctx.sink.is_cancelled() {
                return Err(ERR_CANCELLED.to_string());
            }
            ctx.sink
                .progress("stem", ctx.work_offset + frame + 1, ctx.total_work);
        }
    }
    Ok(())
}

/// Renders the mix (or dub stem) of `original` and `clips` into `output` (WAV).
/// Writes `<output>.partial` first and renames on success; on failure or
/// cancellation the partial file is removed and the original is untouched.
pub fn render(
    original: &Path,
    clips: &[MixClip],
    output: &Path,
    options: &MixOptions,
    sink: &dyn ProgressSink,
) -> Result<MixReport, String> {
    options.validate()?;
    if same_file(original, output) || original == output {
        return Err("output_path must not be the original media file".to_string());
    }
    let partial = partial_path(output);
    let result = render_to(original, clips, &partial, output, options, sink);
    match result {
        Ok(report) => {
            fs::rename(&partial, output).map_err(|e| {
                let _ = fs::remove_file(&partial);
                e.to_string()
            })?;
            Ok(report)
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

fn render_to(
    original: &Path,
    clips: &[MixClip],
    partial: &Path,
    output: &Path,
    options: &MixOptions,
    sink: &dyn ProgressSink,
) -> Result<MixReport, String> {
    let mut reader = open_wav(original)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let original_frames = reader.duration() as u64;
    let analyse = options.loudness_match && !clips.is_empty();
    let total_guess = original_frames * if analyse { 2 } else { 1 };

    let levels = if analyse {
        let levels = match spec.sample_format {
            SampleFormat::Float => analyse_source::<f32, _>(&mut reader, sink, total_guess)?,
            SampleFormat::Int => analyse_source::<i32, _>(&mut reader, sink, total_guess)?,
        };
        reader.seek(0).map_err(|e| e.to_string())?;
        Some(levels)
    } else {
        None
    };

    let mut prepared = Vec::with_capacity(clips.len());
    for clip in clips {
        if sink.is_cancelled() {
            return Err(ERR_CANCELLED.to_string());
        }
        let source_level = levels.as_ref().and_then(|levels| {
            levels.window_dbfs(
                ms_to_frames(clip.src_start_ms, spec.sample_rate),
                ms_to_frames(clip.src_end_ms, spec.sample_rate),
            )
        });
        prepared.push(prepare_clip(
            clip,
            spec.sample_rate,
            channels,
            source_level,
            options,
        )?);
    }
    prepared.sort_by_key(|clip| clip.start_frame);
    let output_frames = prepared
        .iter()
        .map(|clip| clip.start_frame + clip.frames())
        .fold(original_frames, u64::max);
    let reports = prepared
        .iter()
        .map(|c| c.report.clone())
        .collect::<Vec<_>>();
    let ctx = RenderCtx {
        clips: prepared,
        options,
        sink,
        total_work: output_frames + if analyse { original_frames } else { 0 },
        work_offset: if analyse { original_frames } else { 0 },
    };

    let mut limited = 0;
    match options.kind {
        ExportKind::Mix => {
            let file = fs::File::create(partial).map_err(|e| e.to_string())?;
            let mut writer =
                WavWriter::new(BufWriter::new(file), spec).map_err(|e| e.to_string())?;
            limited = match spec.sample_format {
                SampleFormat::Float => {
                    render_mix_pass::<f32, _, _>(&mut reader, &mut writer, &ctx, output_frames)?
                }
                SampleFormat::Int => {
                    render_mix_pass::<i32, _, _>(&mut reader, &mut writer, &ctx, output_frames)?
                }
            };
            writer.finalize().map_err(|e| e.to_string())?;
        }
        ExportKind::DubStem => {
            let stem_spec = WavSpec {
                channels: spec.channels,
                sample_rate: spec.sample_rate,
                bits_per_sample: 32,
                sample_format: SampleFormat::Float,
            };
            let file = fs::File::create(partial).map_err(|e| e.to_string())?;
            let mut writer =
                WavWriter::new(BufWriter::new(file), stem_spec).map_err(|e| e.to_string())?;
            render_stem_pass(&mut writer, &ctx, channels, output_frames)?;
            writer.finalize().map_err(|e| e.to_string())?;
        }
    }
    sink.progress("done", ctx.total_work, ctx.total_work);

    Ok(MixReport {
        output_path: output.display().to_string(),
        kind: options.kind,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        original_frames,
        output_frames,
        limited_frames: limited,
        clips: reports,
    })
}

// ---------------------------------------------------------------------------
// Command core (DB → plan → clips)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ExportInputs {
    pub original_path: PathBuf,
    pub plan: Vec<Placement>,
    pub clips: Vec<MixClip>,
}

/// Reads the media path and dub inputs, places them and builds the clip list.
/// Runs under the DB lock; rendering then runs without it.
pub fn export_inputs(
    conn: &Connection,
    media_id: i64,
    params: &PlacementParams,
) -> Result<ExportInputs, String> {
    let original_path: String = conn
        .query_row(
            "SELECT path FROM media_files WHERE id = ?1",
            params![media_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "media not found".to_string())?;
    let rows = placement::load_dub_sentences(conn, media_id).map_err(|e| e.to_string())?;
    let sentences = rows.iter().map(|r| r.sentence.clone()).collect::<Vec<_>>();
    let plan = placement::place(&sentences, params)?;
    let clips = rows
        .iter()
        .zip(&plan)
        .filter(|(_, placement)| placement.is_placed())
        .filter_map(|(row, placement)| {
            row.tts_path.as_ref().map(|path| MixClip {
                segment_id: row.sentence.segment_id,
                path: PathBuf::from(path),
                dub_start_ms: placement.dub_start_ms,
                src_start_ms: row.sentence.src_start_ms,
                src_end_ms: row.sentence.src_end_ms,
                expected_duration_ms: row.sentence.clip_duration_ms,
            })
        })
        .collect();
    Ok(ExportInputs {
        original_path: PathBuf::from(original_path),
        plan,
        clips,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("tsubame-mix-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_i16(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let spec = WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut w = WavWriter::create(path, spec).unwrap();
        samples.iter().for_each(|s| w.write_sample(*s).unwrap());
        w.finalize().unwrap();
    }

    fn write_f32(path: &Path, rate: u32, channels: u16, samples: &[f32]) {
        let spec = WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut w = WavWriter::create(path, spec).unwrap();
        samples.iter().for_each(|s| w.write_sample(*s).unwrap());
        w.finalize().unwrap();
    }

    fn read_i16(path: &Path) -> (WavSpec, Vec<i16>) {
        let mut r = WavReader::open(path).unwrap();
        let spec = r.spec();
        (spec, r.samples::<i16>().map(|s| s.unwrap()).collect())
    }

    fn read_f32(path: &Path) -> Vec<f32> {
        let mut r = WavReader::open(path).unwrap();
        r.samples::<f32>().map(|s| s.unwrap()).collect()
    }

    /// Deterministic noise in [-amp, amp].
    fn noise(len: usize, amp: f64, seed: u64) -> Vec<i16> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let unit = (state % 20001) as f64 / 10000.0 - 1.0;
                (unit * amp * 32767.0) as i16
            })
            .collect()
    }

    fn raw_options() -> MixOptions {
        MixOptions {
            limiter: false,
            loudness_match: false,
            clip_peak_dbfs: None,
            fade_ms: 0.0,
            ..MixOptions::default()
        }
    }

    fn clip(id: i64, path: PathBuf, start_ms: i64) -> MixClip {
        MixClip {
            segment_id: id,
            path,
            dub_start_ms: start_ms,
            src_start_ms: 0,
            src_end_ms: 1000,
            expected_duration_ms: None,
        }
    }

    #[test]
    fn identity_when_no_clips() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        let samples = noise(8000 * 2, 0.9, 3);
        write_i16(&original, 8000, 2, &samples);
        for limiter in [false, true] {
            let out = dir.path("out.wav");
            let options = MixOptions {
                limiter,
                ..MixOptions::default()
            };
            let report = render(&original, &[], &out, &options, &NoProgress).unwrap();
            let (spec, written) = read_i16(&out);
            assert_eq!(spec, WavReader::open(&original).unwrap().spec());
            assert_eq!(written, samples);
            assert_eq!(report.limited_frames, 0);
        }
    }

    #[test]
    fn original_is_bit_exact_outside_clips_with_limiter_off() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        let rate = 8000;
        let samples = noise(rate as usize * 3 * 2, 0.95, 11);
        write_i16(&original, rate, 2, &samples);
        let dub = dir.path("dub.wav");
        write_f32(&dub, rate, 1, &vec![0.3; 4000]);
        let out = dir.path("out.wav");
        let options = MixOptions {
            limiter: false,
            ..MixOptions::default()
        };
        let report = render(
            &original,
            &[clip(1, dub, 1000)],
            &out,
            &options,
            &NoProgress,
        )
        .unwrap();
        let (_, written) = read_i16(&out);
        let start = report.clips[0].start_frame as usize;
        let end = start + report.clips[0].frames as usize;
        assert_eq!((start, end), (8000, 12000));
        for frame in 0..samples.len() / 2 {
            let inside = frame >= start && frame < end;
            for ch in 0..2 {
                let i = frame * 2 + ch;
                if !inside {
                    assert_eq!(written[i], samples[i], "frame {frame}");
                }
            }
        }
        // The dub really is in the clip region.
        assert!((start..end).any(|f| written[f * 2] != samples[f * 2]));
        // The original file itself is untouched.
        assert_eq!(read_i16(&original).1, samples);
    }

    #[test]
    fn limiter_on_leaves_original_only_frames_untouched() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        let rate = 8000;
        // Original peaks at 0.97 everywhere (above the -1 dBFS ceiling).
        let samples = noise(rate as usize * 2, 0.97, 5);
        write_i16(&original, rate, 1, &samples);
        let dub = dir.path("dub.wav");
        write_f32(&dub, rate, 1, &vec![0.9; 800]);
        let out = dir.path("out.wav");
        let options = MixOptions {
            loudness_match: false,
            clip_peak_dbfs: None,
            ..MixOptions::default()
        };
        render(&original, &[clip(1, dub, 500)], &out, &options, &NoProgress).unwrap();
        let (_, written) = read_i16(&out);
        // Before the clip and well after its release tail: bit-exact.
        assert_eq!(written[..4000], samples[..4000]);
        // The release tail (50 ms at 8 kHz, until gain is within 1e-6 of unity)
        // is part of the sum bus; well after it the original is bit-exact again.
        assert_eq!(written[4800 + 6000..], samples[4800 + 6000..]);
    }

    #[test]
    fn limiter_acts_on_sum_bus_only() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        let rate = 8000;
        write_f32(&original, rate, 1, &vec![0.5; 16000]);
        let dub = dir.path("dub.wav");
        write_f32(&dub, rate, 1, &vec![0.6; 4000]);
        let ceiling = db_to_gain(-1.0);

        let out = dir.path("mix.wav");
        let options = MixOptions {
            loudness_match: false,
            clip_peak_dbfs: None,
            fade_ms: 0.0,
            ..MixOptions::default()
        };
        let report = render(
            &original,
            &[clip(1, dub.clone(), 500)],
            &out,
            &options,
            &NoProgress,
        )
        .unwrap();
        let mixed = read_f32(&out);
        assert!(report.limited_frames >= 4000);
        assert!(mixed[4000..8000]
            .iter()
            .all(|v| (*v as f64) <= ceiling + 1e-6));
        assert!(mixed[..4000].iter().all(|v| *v == 0.5));

        // Limiter off: plain sum.
        let off = dir.path("off.wav");
        let options_off = MixOptions {
            limiter: false,
            ..options.clone()
        };
        render(
            &original,
            &[clip(1, dub.clone(), 500)],
            &off,
            &options_off,
            &NoProgress,
        )
        .unwrap();
        assert!(read_f32(&off)[4000..8000]
            .iter()
            .all(|v| (*v - 1.1).abs() < 1e-6));

        // The stem never sees the limiter.
        let stem = dir.path("stem.wav");
        let stem_options = MixOptions {
            kind: ExportKind::DubStem,
            ..options
        };
        render(
            &original,
            &[clip(1, dub, 500)],
            &stem,
            &stem_options,
            &NoProgress,
        )
        .unwrap();
        assert!(read_f32(&stem)[4000..8000]
            .iter()
            .all(|v| (*v - 0.6).abs() < 1e-6));
    }

    #[test]
    fn clip_offset_is_exact_in_samples() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        let rate = 44_100;
        write_i16(&original, rate, 1, &vec![0; rate as usize]);
        let dub = dir.path("dub.wav");
        write_f32(&dub, rate, 1, &vec![0.25; 100]);
        let out = dir.path("out.wav");
        render(
            &original,
            &[clip(1, dub, 333)],
            &out,
            &raw_options(),
            &NoProgress,
        )
        .unwrap();
        let (_, written) = read_i16(&out);
        let expected_start = (333 * 44_100 + 500) / 1000; // 14685
        let first = written.iter().position(|v| *v != 0).unwrap();
        let last = written.iter().rposition(|v| *v != 0).unwrap();
        assert_eq!(first, expected_start);
        assert_eq!(last, expected_start + 99);
        assert_eq!(written[first], (0.25f64 * 32768.0).round() as i16);
    }

    #[test]
    fn resample_preserves_duration_and_pitch() {
        let from = 24_000;
        let to = 48_000;
        let freq = 440.0;
        let input = (0..24_000)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / from as f64).sin() * 0.5)
            .collect::<Vec<_>>();
        let output = resample(&input, from, to);
        assert_eq!(output.len(), 48_000);
        let crossings = |s: &[f64]| s.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        let middle_in = crossings(&input[2000..22000]) as f64 / (20000.0 / from as f64);
        let middle_out = crossings(&output[4000..44000]) as f64 / (40000.0 / to as f64);
        assert!(
            (middle_in - middle_out).abs() <= 1.0,
            "{middle_in} vs {middle_out}"
        );
        // Amplitude is preserved in the steady state.
        let peak = output[4000..44000]
            .iter()
            .fold(0.0f64, |a, v| a.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
        assert_eq!(resampled_len(22_050, 22_050, 48_000), 48_000);
        assert_eq!(resample(&input[..7], 44_100, 16_000).len(), 3);
    }

    #[test]
    fn mixed_clip_keeps_its_duration_after_resampling() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        write_i16(&original, 48_000, 2, &vec![0; 48_000 * 2 * 2]);
        let dub = dir.path("dub.wav");
        write_f32(&dub, 22_050, 1, &vec![0.2; 22_050]); // exactly 1 s
        let out = dir.path("out.wav");
        let report = render(
            &original,
            &[clip(1, dub, 100)],
            &out,
            &raw_options(),
            &NoProgress,
        )
        .unwrap();
        assert_eq!(report.clips[0].frames, 48_000);
        assert_eq!(report.clips[0].start_frame, 4800);
        assert_eq!(report.output_frames, 96_000);
    }

    #[test]
    fn loudness_target_is_source_plus_offset_clamped() {
        let options = MixOptions::default();
        assert_eq!(loudness_target_dbfs(Some(-30.0), &options), -38.0);
        assert_eq!(loudness_target_dbfs(Some(-10.0), &options), -30.0);
        assert_eq!(loudness_target_dbfs(Some(-60.0), &options), -42.0);
        assert_eq!(loudness_target_dbfs(None, &options), -42.0);
        // Gate ignores silence around speech.
        let mut mono = vec![0.0; 8000];
        mono.extend(vec![0.1; 8000]);
        let level = active_rms_dbfs(&mono, 8000).unwrap();
        assert!((level - 20.0 * 0.1f64.log10()).abs() < 1e-9);
        assert_eq!(active_rms_dbfs(&vec![0.0; 100], 8000), None);
    }

    #[test]
    fn loudness_match_applies_clamped_gain_and_max_boost() {
        let dir = TempDir::new();
        let rate = 8000;
        let original = dir.path("orig.wav");
        // Source sentence at 0..1000 ms is loud (0.5 RMS ≈ -6 dBFS).
        let mut source = vec![0.5f32; rate as usize];
        source.extend(vec![0.0f32; rate as usize * 2]);
        write_f32(&original, rate, 1, &source);
        let loud = dir.path("loud.wav");
        write_f32(&loud, rate, 1, &vec![0.1; 4000]); // -20 dBFS
        let quiet = dir.path("quiet.wav");
        write_f32(&quiet, rate, 1, &vec![0.0005; 4000]); // ≈ -66 dBFS
        let options = MixOptions {
            limiter: false,
            clip_peak_dbfs: None,
            fade_ms: 0.0,
            ..MixOptions::default()
        };
        let mut silent_src = clip(2, quiet, 2000);
        silent_src.src_start_ms = 1500;
        silent_src.src_end_ms = 2000;
        let out = dir.path("out.wav");
        let report = render(
            &original,
            &[clip(1, loud, 1000), silent_src],
            &out,
            &options,
            &NoProgress,
        )
        .unwrap();
        let first = &report.clips[0];
        // -6 + -8 = -14 → clamped to -30.
        assert_eq!(first.target_active_rms_dbfs, Some(-30.0));
        assert!((first.gain_db - (-10.0)).abs() < 1e-6);
        let written = read_f32(&out);
        let level = active_rms_dbfs(
            &written[8000..12000]
                .iter()
                .map(|v| *v as f64 - 0.0)
                .collect::<Vec<_>>(),
            rate,
        )
        .unwrap();
        assert!((level + 30.0).abs() < 0.01, "level {level}");
        // Silent source → minimum (-42); boost capped at +12 dB.
        let second = &report.clips[1];
        assert_eq!(second.source_active_rms_dbfs, None);
        assert_eq!(second.target_active_rms_dbfs, Some(-42.0));
        assert!((second.gain_db - 12.0).abs() < 1e-9);
    }

    #[test]
    fn stem_export_contains_only_the_dub() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        write_i16(&original, 8000, 2, &noise(8000 * 2, 0.5, 9));
        let dub = dir.path("dub.wav");
        write_f32(&dub, 8000, 1, &vec![0.25; 800]);
        let stem = dir.path("stem.wav");
        let options = MixOptions {
            kind: ExportKind::DubStem,
            ..raw_options()
        };
        let report = render(
            &original,
            &[clip(1, dub, 250)],
            &stem,
            &options,
            &NoProgress,
        )
        .unwrap();
        let spec = WavReader::open(&stem).unwrap().spec();
        assert_eq!(
            (spec.sample_format, spec.channels, spec.sample_rate),
            (SampleFormat::Float, 2, 8000)
        );
        let samples = read_f32(&stem);
        assert_eq!(samples.len() as u64, report.output_frames * 2);
        for (frame, pair) in samples.chunks(2).enumerate() {
            let expected = if (2000..2800).contains(&frame) {
                0.25
            } else {
                0.0
            };
            assert_eq!(pair, [expected, expected], "frame {frame}");
        }
    }

    #[test]
    fn dub_past_the_end_extends_instead_of_truncating() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        let samples = noise(8000, 0.3, 1);
        write_i16(&original, 8000, 1, &samples);
        let dub = dir.path("dub.wav");
        write_f32(&dub, 8000, 1, &vec![0.1; 4000]);
        let out = dir.path("out.wav");
        let report = render(
            &original,
            &[clip(1, dub, 900)],
            &out,
            &raw_options(),
            &NoProgress,
        )
        .unwrap();
        assert_eq!(report.output_frames, 7200 + 4000);
        let (_, written) = read_i16(&out);
        assert_eq!(written.len(), 11_200);
        assert_eq!(written[..7200], samples[..7200]);
        assert_eq!(written[11_199], (0.1f64 * 32768.0).round() as i16);
    }

    #[test]
    fn refuses_to_overwrite_the_original_and_cleans_up_on_cancel() {
        let dir = TempDir::new();
        let original = dir.path("orig.wav");
        write_i16(&original, 8000, 1, &noise(200_000, 0.3, 2));
        assert!(render(
            &original,
            &[],
            &original,
            &MixOptions::default(),
            &NoProgress
        )
        .is_err());

        struct Cancel(AtomicBool);
        impl ProgressSink for Cancel {
            fn progress(&self, _: &str, _: u64, _: u64) {
                self.0.store(true, Ordering::SeqCst);
            }
            fn is_cancelled(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }
        let out = dir.path("out.wav");
        let error = render(
            &original,
            &[],
            &out,
            &raw_options(),
            &Cancel(AtomicBool::new(false)),
        )
        .unwrap_err();
        assert_eq!(error, ERR_CANCELLED);
        assert!(!out.exists());
        assert!(!partial_path(&out).exists());
    }

    #[test]
    fn non_wav_and_duration_mismatch_fail_loudly() {
        let dir = TempDir::new();
        let original = dir.path("orig.mp3");
        fs::write(&original, b"ID3 not a wav").unwrap();
        let error = render(
            &original,
            &[],
            &dir.path("o.wav"),
            &raw_options(),
            &NoProgress,
        )
        .unwrap_err();
        assert!(error.starts_with("unsupported_source_format"));

        let wav = dir.path("orig.wav");
        write_i16(&wav, 8000, 1, &vec![0; 8000]);
        let dub = dir.path("dub.wav");
        write_f32(&dub, 8000, 1, &vec![0.1; 800]); // 100 ms
        let mut wrong = clip(1, dub, 0);
        wrong.expected_duration_ms = Some(400);
        let error = render(
            &wav,
            &[wrong],
            &dir.path("o.wav"),
            &raw_options(),
            &NoProgress,
        )
        .unwrap_err();
        assert!(error.starts_with("clip_duration_mismatch"));
    }

    #[test]
    fn options_are_validated() {
        assert!(MixOptions::default().validate().is_ok());
        assert!(MixOptions {
            limiter_ceiling_dbfs: 0.5,
            ..MixOptions::default()
        }
        .validate()
        .is_err());
        assert!(MixOptions {
            min_active_rms_dbfs: -10.0,
            ..MixOptions::default()
        }
        .validate()
        .is_err());
        let parsed: MixOptions =
            serde_json::from_str(r#"{"limiter":false,"kind":"dub_stem"}"#).unwrap();
        assert!(!parsed.limiter);
        assert_eq!(parsed.kind, ExportKind::DubStem);
        assert_eq!(parsed.relative_loudness_db, -8.0);
    }

    #[test]
    fn export_inputs_build_clips_from_the_plan() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media_files (id INTEGER PRIMARY KEY, path TEXT);
             INSERT INTO media_files VALUES (1, '/m/a.wav');
             CREATE TABLE segments (id INTEGER PRIMARY KEY, media_id INTEGER, start_ms INTEGER,
               end_ms INTEGER, ordinal INTEGER DEFAULT 0, tts_asset_key TEXT,
               dub_skip INTEGER DEFAULT 0);
             CREATE TABLE tts_assets (key TEXT PRIMARY KEY, path TEXT, duration_ms INTEGER);
             INSERT INTO tts_assets VALUES ('a', '/t/a.wav', 3000), ('b', '/t/b.wav', 500);
             INSERT INTO segments VALUES (1, 1, 0, 1000, 0, 'a', 0);
             INSERT INTO segments VALUES (2, 1, 1100, 1200, 1, 'b', 0);
             INSERT INTO segments VALUES (3, 1, 1300, 1400, 2, 'b', 1);",
        )
        .unwrap();
        let inputs = export_inputs(&conn, 1, &PlacementParams::default()).unwrap();
        assert_eq!(inputs.original_path, PathBuf::from("/m/a.wav"));
        assert_eq!(inputs.plan.len(), 3);
        assert_eq!(inputs.clips.len(), 2);
        assert_eq!(inputs.clips[0].dub_start_ms, 1300);
        assert_eq!(inputs.clips[1].dub_start_ms, 1300 + 3000 + 120);
        assert_eq!(inputs.clips[1].expected_duration_ms, Some(500));
        assert!(export_inputs(&conn, 9, &PlacementParams::default()).is_err());
    }
}
