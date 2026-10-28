//! Pure-Rust decode of imported source media to PCM WAV (symphonia, MPL-2.0).
//!
//! Stages that need a WAV (local faster-whisper ASR, the dub mixer) go through
//! [ensure_wav]: a source that already satisfies the stage's
//! [DecodeTarget] is used as-is; anything else (mp3, m4a/mp4/m4b, aac,
//! flac, ogg/vorbis, mkv/webm audio, alac, adpcm, non-PCM WAV) is decoded once
//! into the app-owned decode cache, keyed by sha256(source path, size, mtime,
//! target, decoder version), and reused by later jobs. Failures are typed
//! [DecodeError]s with a stable code; there is no silent fallback. Opus has
//! no non-GPL pure-Rust decoder available, so it fails with unsupported_codec.

use crate::jobs::ProgressSink;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecType, DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Bump when the decoder output for the same input can change; it is part of
/// the cache key, so old cache files are simply no longer hit.
const DECODER_VERSION: u32 = 1;

static CACHE_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Sets the decode cache directory (<app data>/cache/decoded). Called once
/// from the app setup hook; later calls are ignored.
pub fn init_cache_root(dir: PathBuf) {
    let _ = CACHE_ROOT.set(dir);
}

/// The PCM a stage needs from the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeTarget {
    /// Some(rate): resample to rate and require 16-bit integer PCM;
    /// None: keep the source rate.
    pub sample_rate: Option<u32>,
    /// Downmix to one channel (mean of all channels).
    pub mono: bool,
}

impl DecodeTarget {
    /// Local ASR input (faster-whisper worker): 16 kHz, mono, 16-bit.
    pub const ASR_16K_MONO: Self = Self {
        sample_rate: Some(16_000),
        mono: true,
    };
    /// Mixer input: the source's own rate and channel layout.
    pub const NATIVE: Self = Self {
        sample_rate: None,
        mono: false,
    };
}

/// What a decode wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    pub duration_ms: i64,
    /// Short codec name (mp3, aac, flac, ...).
    pub codec: String,
    /// Corrupt packets the decoder skipped (the rest of the stream decoded).
    pub skipped_packets: u64,
}

/// The WAV a stage should read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub path: PathBuf,
    /// true when path is a decoded cache file rather than the source.
    pub decoded: bool,
    /// true when the cache file already existed (no decode ran).
    pub reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    SourceMissing(String),
    UnsupportedFormat(String),
    UnsupportedCodec { codec: String, path: String },
    NoAudioTrack(String),
    DecodeFailed(String),
    Empty(String),
    Io(String),
    CacheUnavailable,
    Cancelled,
}

impl DecodeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SourceMissing(_) => "source_missing",
            Self::UnsupportedFormat(_) => "unsupported_source_format",
            Self::UnsupportedCodec { .. } => "unsupported_codec",
            Self::NoAudioTrack(_) => "no_audio_track",
            Self::DecodeFailed(_) => "decode_failed",
            Self::Empty(_) => "decode_empty",
            Self::Io(_) => "decode_io",
            Self::CacheUnavailable => "decode_cache_unavailable",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            Self::SourceMissing(detail) => write!(f, "{code}: cannot open source media {detail}"),
            Self::UnsupportedFormat(detail) => {
                write!(f, "{code}: container not recognised ({detail})")
            }
            Self::UnsupportedCodec { codec, path } => write!(
                f,
                "{code}: {path} uses the {codec} codec, which this build cannot decode"
            ),
            Self::NoAudioTrack(path) => write!(f, "{code}: {path} has no audio track"),
            Self::DecodeFailed(detail) => write!(f, "{code}: {detail}"),
            Self::Empty(path) => write!(f, "{code}: {path} decodes to no audio"),
            Self::Io(detail) => write!(f, "{code}: {detail}"),
            Self::CacheUnavailable => {
                write!(f, "{code}: decode cache directory is not initialised")
            }
            Self::Cancelled => write!(f, "{code}"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn io_err(context: &str, error: impl fmt::Display) -> DecodeError {
    DecodeError::Io(format!("{context}: {error}"))
}

/// The WAV src should be read as for target: src itself when it already
/// fits, otherwise a (cached) decode. Runs on the caller's (job) thread and
/// checks sink.is_cancelled() between packets.
pub fn ensure_wav(
    src: &Path,
    target: DecodeTarget,
    sink: &dyn ProgressSink,
) -> Result<Prepared, DecodeError> {
    prepare(CACHE_ROOT.get().map(PathBuf::as_path), src, target, sink)
}

fn prepare(
    cache_root: Option<&Path>,
    src: &Path,
    target: DecodeTarget,
    sink: &dyn ProgressSink,
) -> Result<Prepared, DecodeError> {
    let meta = fs::metadata(src)
        .map_err(|e| DecodeError::SourceMissing(format!("{}: {e}", src.display())))?;
    if is_compatible_wav(src, target) {
        return Ok(Prepared {
            path: src.to_path_buf(),
            decoded: false,
            reused: false,
        });
    }
    let root = cache_root.ok_or(DecodeError::CacheUnavailable)?;
    let key = cache_key(src, &meta, target);
    let dest = root.join(&key[..2]).join(format!("{key}.wav"));
    if dest.is_file() {
        return Ok(Prepared {
            path: dest,
            decoded: true,
            reused: true,
        });
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err("cannot create decode cache", e))?;
    }
    decode_to_pcm_wav(src, &dest, target, sink)?;
    Ok(Prepared {
        path: dest,
        decoded: true,
        reused: false,
    })
}

/// A WAV hound reads whose layout already matches target.
fn is_compatible_wav(src: &Path, target: DecodeTarget) -> bool {
    let Ok(reader) = WavReader::open(src) else {
        return false;
    };
    let spec = reader.spec();
    if reader.duration() == 0 || (target.mono && spec.channels != 1) {
        return false;
    }
    match target.sample_rate {
        Some(rate) => {
            spec.sample_rate == rate
                && spec.bits_per_sample == 16
                && spec.sample_format == SampleFormat::Int
        }
        None => true,
    }
}

fn cache_key(src: &Path, meta: &fs::Metadata, target: DecodeTarget) -> String {
    let path = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(meta.len().to_le_bytes());
    hasher.update(mtime.to_le_bytes());
    hasher.update(target.sample_rate.unwrap_or(0).to_le_bytes());
    hasher.update([target.mono as u8]);
    hasher.update(DECODER_VERSION.to_le_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Decodes src into a 16-bit PCM WAV at dest in the target layout.
/// Writes a unique .partial sibling first and renames on success, so an
/// error or cancel never leaves a truncated dest.
pub fn decode_to_pcm_wav(
    src: &Path,
    dest: &Path,
    target: DecodeTarget,
    sink: &dyn ProgressSink,
) -> Result<DecodedInfo, DecodeError> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "decoded.wav".into());
    let partial = dest.with_file_name(format!("{name}.{}.partial", uuid::Uuid::new_v4()));
    match decode_into(src, &partial, target, sink) {
        Ok(info) => {
            fs::rename(&partial, dest).map_err(|e| {
                let _ = fs::remove_file(&partial);
                io_err("cannot finalise decoded WAV", e)
            })?;
            Ok(info)
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}

fn codec_name(codec: CodecType) -> String {
    if codec == CODEC_TYPE_OPUS {
        return "opus".into();
    }
    symphonia::default::get_codecs()
        .get_codec(codec)
        .map(|d| d.short_name.to_string())
        .unwrap_or_else(|| format!("{codec:?}"))
}

fn decode_into(
    src: &Path,
    out: &Path,
    target: DecodeTarget,
    sink: &dyn ProgressSink,
) -> Result<DecodedInfo, DecodeError> {
    let shown = src.display().to_string();
    let file =
        fs::File::open(src).map_err(|e| DecodeError::SourceMissing(format!("{shown}: {e}")))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = src.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| DecodeError::UnsupportedFormat(format!("{shown}: {e}")))?;
    let mut format = probed.format;

    // First track with a decoder in this build; a track whose codec has none
    // (Opus, video-only codecs) is reported by name.
    let registry = symphonia::default::get_codecs();
    let (track_id, params) = {
        let tracks = format.tracks();
        match tracks.iter().find(|t| {
            t.codec_params.codec != CODEC_TYPE_NULL
                && registry.get_codec(t.codec_params.codec).is_some()
        }) {
            Some(track) => (track.id, track.codec_params.clone()),
            None => {
                return Err(
                    match tracks
                        .iter()
                        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
                    {
                        Some(track) => DecodeError::UnsupportedCodec {
                            codec: codec_name(track.codec_params.codec),
                            path: shown,
                        },
                        None => DecodeError::NoAudioTrack(shown),
                    },
                )
            }
        }
    };
    let codec = codec_name(params.codec);
    let mut decoder = registry
        .make(&params, &DecoderOptions::default())
        .map_err(|e| match e {
            SymError::Unsupported(_) => DecodeError::UnsupportedCodec {
                codec: codec.clone(),
                path: shown.clone(),
            },
            other => DecodeError::DecodeFailed(format!("{shown}: {other}")),
        })?;

    let mut writer: Option<WavWriter<BufWriter<fs::File>>> = None;
    let mut resampler: Option<Resampler> = None;
    let mut src_rate = 0u32;
    let mut src_channels = 0usize;
    let mut out_channels = 0usize;
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut buf_frames = 0usize;
    let mut mixed: Vec<f32> = Vec::new();
    let mut resampled: Vec<f32> = Vec::new();
    let mut decoded_packets = 0u64;
    let mut skipped_packets = 0u64;
    let mut samples_out = 0u64;

    loop {
        if sink.is_cancelled() {
            return Err(DecodeError::Cancelled);
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => {
                return Err(DecodeFailed_chained(&shown));
            }
            Err(e) => return Err(DecodeError::DecodeFailed(format!("{shown}: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymError::DecodeError(_)) => {
                skipped_packets += 1;
                continue;
            }
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(DecodeError::DecodeFailed(format!("{shown}: {e}"))),
        };
        let spec = decoded.spec().to_owned();
        if decoded.frames() == 0 {
            continue;
        }
        if writer.is_none() {
            src_rate = spec.rate;
            src_channels = spec.channels.count().max(1);
            out_channels = if target.mono { 1 } else { src_channels };
            let out_rate = target.sample_rate.unwrap_or(src_rate);
            if src_rate == 0 || out_rate == 0 || out_channels > u16::MAX as usize {
                return Err(DecodeError::DecodeFailed(format!(
                    "{shown}: invalid stream layout ({src_rate} Hz, {src_channels} channels)"
                )));
            }
            let spec_out = WavSpec {
                channels: out_channels as u16,
                sample_rate: out_rate,
                bits_per_sample: 16,
                sample_format: SampleFormat::Int,
            };
            writer = Some(
                WavWriter::create(out, spec_out)
                    .map_err(|e| io_err("cannot create decoded WAV", e))?,
            );
            resampler = Some(Resampler::new(src_rate, out_rate, out_channels));
        } else if spec.rate != src_rate || spec.channels.count().max(1) != src_channels {
            return Err(DecodeError::DecodeFailed(format!(
                "{shown}: stream changes sample rate or channel count mid-file"
            )));
        }
        decoded_packets += 1;

        let capacity = decoded.capacity();
        if sample_buf.is_none() || capacity > buf_frames {
            sample_buf = Some(SampleBuffer::<f32>::new(capacity as u64, spec));
            buf_frames = capacity;
        }
        let buf = sample_buf.as_mut().expect("sample buffer allocated above");
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();

        let frame_samples: &[f32] = if target.mono && src_channels > 1 {
            mixed.clear();
            mixed.extend(
                samples
                    .chunks_exact(src_channels)
                    .map(|frame| frame.iter().sum::<f32>() / src_channels as f32),
            );
            &mixed
        } else {
            samples
        };
        resampled.clear();
        resampler
            .as_mut()
            .expect("resampler created with the writer")
            .push(frame_samples, &mut resampled);
        write_samples(writer.as_mut().expect("writer created above"), &resampled)?;
        samples_out += resampled.len() as u64;
    }

    let (Some(mut writer), Some(mut resampler)) = (writer, resampler) else {
        return Err(if skipped_packets > 0 {
            DecodeError::DecodeFailed(format!(
                "{shown}: every {codec} packet failed to decode ({skipped_packets} packets)"
            ))
        } else {
            DecodeError::Empty(shown)
        });
    };
    resampled.clear();
    resampler.finish(&mut resampled);
    write_samples(&mut writer, &resampled)?;
    samples_out += resampled.len() as u64;
    let spec_out = writer.spec();
    writer
        .finalize()
        .map_err(|e| io_err("cannot finalise decoded WAV", e))?;

    let frames = samples_out / out_channels.max(1) as u64;
    if frames == 0 || decoded_packets == 0 {
        return Err(DecodeError::Empty(shown));
    }
    Ok(DecodedInfo {
        sample_rate: spec_out.sample_rate,
        channels: spec_out.channels,
        frames,
        duration_ms: ((frames as u128).saturating_mul(1000) / spec_out.sample_rate as u128) as i64,
        codec,
        skipped_packets,
    })
}

#[allow(non_snake_case)]
fn DecodeFailed_chained(shown: &str) -> DecodeError {
    DecodeError::DecodeFailed(format!("{shown}: chained/reset streams are not supported"))
}

fn write_samples(
    writer: &mut WavWriter<BufWriter<fs::File>>,
    samples: &[f32],
) -> Result<(), DecodeError> {
    for &sample in samples {
        let value = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        writer
            .write_sample(value)
            .map_err(|e| io_err("cannot write decoded WAV", e))?;
    }
    Ok(())
}

/// Streaming linear-interpolation resampler over interleaved frames. Output
/// frame k sits at input position k * in_rate / out_rate, computed in
/// integers so long files do not drift. Equal rates pass through untouched.
struct Resampler {
    in_rate: u64,
    out_rate: u64,
    channels: usize,
    /// Pending interleaved input frames; frame 0 is absolute frame base.
    buf: Vec<f32>,
    base: u64,
    next_out: u64,
    total_in: u64,
}

impl Resampler {
    fn new(in_rate: u32, out_rate: u32, channels: usize) -> Self {
        Self {
            in_rate: in_rate as u64,
            out_rate: out_rate as u64,
            channels: channels.max(1),
            buf: Vec::new(),
            base: 0,
            next_out: 0,
            total_in: 0,
        }
    }

    fn passthrough(&self) -> bool {
        self.in_rate == self.out_rate
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.total_in += (input.len() / self.channels) as u64;
        if self.passthrough() {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        self.emit(out, false);
    }

    fn finish(&mut self, out: &mut Vec<f32>) {
        if !self.passthrough() {
            self.emit(out, true);
        }
    }

    fn emit(&mut self, out: &mut Vec<f32>, last: bool) {
        let ch = self.channels;
        let total_out = if last {
            (self.total_in * self.out_rate).div_ceil(self.in_rate)
        } else {
            u64::MAX
        };
        let available_end = self.base + (self.buf.len() / ch) as u64;
        while self.next_out < total_out {
            let position = self.next_out * self.in_rate;
            let idx = position / self.out_rate;
            if idx >= available_end || (!last && idx + 1 >= available_end) {
                break;
            }
            let frac = (position % self.out_rate) as f32 / self.out_rate as f32;
            let i0 = (idx - self.base) as usize * ch;
            let i1 = if idx + 1 < available_end { i0 + ch } else { i0 };
            for c in 0..ch {
                let a = self.buf[i0 + c];
                let b = self.buf[i1 + c];
                out.push(a + (b - a) * frac);
            }
            self.next_out += 1;
        }
        // Drop input frames no later output frame can reference.
        let needed = self.next_out * self.in_rate / self.out_rate;
        if needed > self.base {
            let drop = ((needed - self.base) as usize).min(self.buf.len() / ch);
            self.buf.drain(..drop * ch);
            self.base += drop as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::NoopSink;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "yuzuki-media-decode-{tag}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// seconds of a 440 Hz sine at rate, 16-bit, channels channels.
    fn write_sine(path: &Path, rate: u32, channels: u16, seconds: f64) {
        let spec = WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut writer = WavWriter::create(path, spec).unwrap();
        let frames = (rate as f64 * seconds) as u64;
        for i in 0..frames {
            let t = i as f64 / rate as f64;
            let value = ((t * 440.0 * std::f64::consts::TAU).sin() * 16_000.0) as i16;
            for _ in 0..channels {
                writer.write_sample(value).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn decodes_44k_stereo_wav_to_16k_mono_pcm16() {
        let dir = TempDir::new("sine");
        let src = dir.0.join("sine.wav");
        write_sine(&src, 44_100, 2, 2.0);
        let dest = dir.0.join("out.wav");

        let info = decode_to_pcm_wav(&src, &dest, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap();
        assert_eq!((info.sample_rate, info.channels), (16_000, 1));
        assert!(
            (info.duration_ms - 2000).abs() <= 20,
            "{}",
            info.duration_ms
        );

        let reader = WavReader::open(&dest).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.sample_rate, 16_000);
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.bits_per_sample, 16);
        assert_eq!(spec.sample_format, SampleFormat::Int);
        let samples: Vec<i16> = reader.into_samples::<i16>().map(Result::unwrap).collect();
        let duration_ms = samples.len() as f64 * 1000.0 / 16_000.0;
        assert!((duration_ms - 2000.0).abs() <= 20.0, "{duration_ms}");
        // The tone survives: RMS of a 16000-peak sine is ~11314.
        let rms = (samples.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / samples.len() as f64)
            .sqrt();
        assert!((rms - 11_314.0).abs() < 600.0, "{rms}");
        // No partial file is left beside the output.
        let leftovers = fs::read_dir(&dir.0)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .to_string_lossy()
                    .ends_with(".partial")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn garbage_and_missing_sources_are_typed_errors() {
        let dir = TempDir::new("garbage");
        let src = dir.0.join("noise.mp3");
        fs::write(&src, b"definitely not an mp3 stream, just ascii bytes").unwrap();
        let dest = dir.0.join("out.wav");
        let error =
            decode_to_pcm_wav(&src, &dest, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap_err();
        assert!(
            matches!(
                error,
                DecodeError::UnsupportedFormat(_)
                    | DecodeError::DecodeFailed(_)
                    | DecodeError::Empty(_)
            ),
            "{error:?}"
        );
        assert!(!dest.exists());

        let missing = prepare(
            Some(&dir.0),
            &dir.0.join("nope.m4a"),
            DecodeTarget::ASR_16K_MONO,
            &NoopSink,
        )
        .unwrap_err();
        assert_eq!(missing.code(), "source_missing");
    }

    #[test]
    fn decode_cache_is_reused_and_keyed_by_source_state() {
        let dir = TempDir::new("cache");
        let cache = dir.0.join("cache");
        let src = dir.0.join("track.wav");
        write_sine(&src, 44_100, 2, 0.5);

        let first = prepare(Some(&cache), &src, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap();
        assert!(first.decoded && !first.reused);
        assert!(first.path.starts_with(&cache));
        let second = prepare(Some(&cache), &src, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap();
        assert!(second.decoded && second.reused);
        assert_eq!(first.path, second.path);

        // A changed source (different size) gets a fresh decode.
        write_sine(&src, 44_100, 2, 0.75);
        let third = prepare(Some(&cache), &src, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap();
        assert!(third.decoded && !third.reused);
        assert_ne!(third.path, first.path);

        // Without a cache root a decode is an explicit error, not a fallback.
        let error = prepare(None, &src, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap_err();
        assert_eq!(error, DecodeError::CacheUnavailable);
    }

    #[test]
    fn compatible_wav_is_used_in_place() {
        let dir = TempDir::new("passthrough");
        let mono16k = dir.0.join("mono16k.wav");
        write_sine(&mono16k, 16_000, 1, 0.25);
        let prepared = prepare(None, &mono16k, DecodeTarget::ASR_16K_MONO, &NoopSink).unwrap();
        assert_eq!(prepared.path, mono16k);
        assert!(!prepared.decoded);

        let stereo = dir.0.join("stereo.wav");
        write_sine(&stereo, 48_000, 2, 0.25);
        let prepared = prepare(None, &stereo, DecodeTarget::NATIVE, &NoopSink).unwrap();
        assert_eq!(prepared.path, stereo);
    }

    #[test]
    fn decode_honours_cancel() {
        struct Cancelled(AtomicBool);
        impl ProgressSink for Cancelled {
            fn progress(&self, _: &str, _: u64, _: u64) {}
            fn is_cancelled(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }
        let dir = TempDir::new("cancel");
        let src = dir.0.join("sine.wav");
        write_sine(&src, 44_100, 2, 0.5);
        let dest = dir.0.join("out.wav");
        let error = decode_to_pcm_wav(
            &src,
            &dest,
            DecodeTarget::ASR_16K_MONO,
            &Cancelled(AtomicBool::new(true)),
        )
        .unwrap_err();
        assert!(error.is_cancelled());
        assert!(!dest.exists());
    }

    #[test]
    fn resampler_length_matches_rate_ratio() {
        let mut resampler = Resampler::new(44_100, 16_000, 1);
        let mut out = Vec::new();
        let input = vec![0.25f32; 44_100];
        for chunk in input.chunks(1_000) {
            resampler.push(chunk, &mut out);
        }
        resampler.finish(&mut out);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }
}
