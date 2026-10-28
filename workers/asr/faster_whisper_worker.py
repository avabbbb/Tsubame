#!/usr/bin/env python3
'''YUZUKI Faster-Whisper worker.

Reads one JSON request from stdin and writes one versioned ASR result to stdout.
The runtime packs own Python/CTranslate2 (runtime.faster-whisper) and the CUDA
libraries (runtime.faster-whisper.cuda); models download through
faster-whisper's Hugging Face cache. This file owns only the worker contract.

CUDA libraries:
- YUZUKI_FW_CUDA_DIR: version dir of the active GPU pack, set by the Desktop
  (app/src-tauri/src/asr.rs). Its nvidia/<lib>/bin dirs are added with
  os.add_dll_directory before ctranslate2 is imported. When it is unset (a
  developer venv), nvidia/<lib>/bin under sys.path is used instead.

Device selection (no silent fallback):
- request.device or YUZUKI_ASR_DEVICE: auto | cuda | cpu (default auto)
- auto: cuda + int8_float16 when CUDA libraries are registered and ctranslate2
  sees a CUDA device, otherwise cpu + int8; the reason is noted.
- cuda: fails with a clear error when the GPU pack is missing or no CUDA
  device is visible. It never switches to cpu.
- request.compute_type or YUZUKI_ASR_COMPUTE_TYPE overrides the compute type.
- YUZUKI_ASR_MODEL_DIR: download/cache root for CTranslate2 models.
The chosen device/compute_type is reported in the result notes.
'''

import glob
import json
import math
import os
import sys

CUDA_ENV = 'YUZUKI_FW_CUDA_DIR'
GPU_PACK = 'runtime.faster-whisper.cuda'

# Friendly ids -> CTranslate2 repos. Anything else is passed through unchanged
# (faster-whisper size names such as large-v3-turbo, or a local model dir).
MODEL_ALIASES = {
    'anime-whisper': 'quantumcookie/anime-whisper-ct2',
    'kotoba-whisper-v2.0': 'kotoba-tech/kotoba-whisper-v2.0-faster',
}

# litagin/anime-whisper: an initial prompt causes heavy hallucination; repeated
# phrases are tamed with no_repeat_ngram_size 5-10 (model card guidance).
NO_PROMPT_MODELS = {'anime-whisper', 'quantumcookie/anime-whisper-ct2'}


def fail(message):
    print(message, file=sys.stderr)
    raise SystemExit(2)


def to_ms(seconds):
    return int(math.floor(float(seconds) / 0.001 + 0.5))


def register_cuda_dlls():
    '''Adds the nvidia/<lib>/bin dirs of the GPU pack (or of a dev venv) to the DLL
    search path. Returns the dirs added; Windows only.'''
    if os.name != 'nt':
        return []
    pack = os.environ.get(CUDA_ENV)
    if pack:
        if not os.path.isdir(pack):
            fail(f'{CUDA_ENV} points to a missing GPU runtime pack: {pack}')
        roots = [pack]
    else:
        roots = list(sys.path)
    added = []
    for root in roots:
        nvidia = os.path.join(root, 'nvidia')
        libs = sorted(os.listdir(nvidia)) if os.path.isdir(nvidia) else []
        for bin_dir in (os.path.join(nvidia, lib, 'bin') for lib in libs):
            if os.path.isdir(bin_dir) and bin_dir not in added:
                os.add_dll_directory(bin_dir)
                os.environ['PATH'] = bin_dir + os.pathsep + os.environ.get('PATH', '')
                added.append(bin_dir)
    if pack and not added:
        fail(f'GPU runtime pack at {pack} has no nvidia/<lib>/bin directories')
    return added


def load_audio(path):
    '''16 kHz mono PCM16 WAV (what YUZUKI's media_decode prepares) -> float32
    array, so faster-whisper never touches PyAV (av>=15 rejects the
    metadata_errors kwarg faster-whisper 1.2.1 passes). Other inputs fall
    back to faster-whisper's own decoder.'''
    import wave

    import numpy as np

    try:
        with wave.open(path, 'rb') as wav:
            if (wav.getframerate(), wav.getnchannels(), wav.getsampwidth()) != (16000, 1, 2):
                return path
            frames = wav.readframes(wav.getnframes())
    except (wave.Error, EOFError, OSError):
        return path
    return np.frombuffer(frames, dtype='<i2').astype(np.float32) / 32768.0


def choose_device(requested, cuda_libs, cuda_devices):
    '''-> (device, note or None). Explicit cuda never becomes cpu.'''
    if requested == 'cpu':
        return 'cpu', None
    if requested == 'cuda':
        if not cuda_libs:
            fail(f'device=cuda was requested but the GPU runtime pack ({GPU_PACK}) is not installed')
        if cuda_devices == 0:
            fail('device=cuda was requested but ctranslate2 sees no CUDA device')
        return 'cuda', None
    if not cuda_libs:
        return 'cpu', f'auto chose cpu: {GPU_PACK} is not installed'
    if cuda_devices == 0:
        return 'cpu', 'auto chose cpu: no CUDA device is visible'
    return 'cuda', None


def choose_compute_type(device, supported):
    '''Default compute type for device. supported: ctranslate2's compute types
    for that device. int8_float16 needs fast FP16 (Volta and newer, e.g. the
    RTX 3050 Ti); Pascal cards such as the GTX 1070 only get int8.'''
    if device != 'cuda':
        return 'int8'
    for candidate in ('int8_float16', 'float16', 'int8'):
        if candidate in supported:
            return candidate
    return 'float32'


def segment_confidence(segment):
    '''Mean word probability of one segment, clamped to 0..1.

    faster-whisper computes avg_logprob once per decoded window and copies it
    to every segment of that window, so adjacent lines report identical
    confidence. Word probabilities are per segment; avg_logprob is only the
    fallback when no words were produced.'''
    words = getattr(segment, 'words', None) or []
    probs = [float(w.probability) for w in words if getattr(w, 'probability', None) is not None]
    if probs:
        return max(0.0, min(1.0, sum(probs) / len(probs)))
    avg_logprob = getattr(segment, 'avg_logprob', None)
    if avg_logprob is None:
        return None
    return max(0.0, min(1.0, math.exp(float(avg_logprob))))


def main():
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        fail(f'invalid worker request: {exc}')

    if request.get('contract_version') != 1:
        fail('unsupported ASR contract version')

    requested_device = str(request.get('device') or os.environ.get('YUZUKI_ASR_DEVICE') or 'auto')
    if requested_device not in ('auto', 'cuda', 'cpu'):
        fail(f'unsupported device: {requested_device}')

    dll_dirs = register_cuda_dlls()
    # Non-Windows hosts resolve CUDA through the system loader.
    cuda_libs = os.name != 'nt' or bool(dll_dirs)

    try:
        import ctranslate2
        from faster_whisper import WhisperModel
    except Exception as exc:
        fail(f'faster-whisper runtime is unavailable: {exc}')

    cuda_devices = ctranslate2.get_cuda_device_count() if cuda_libs else 0
    device, device_note = choose_device(requested_device, cuda_libs, cuda_devices)

    requested_model = str(request.get('model_id') or 'large-v3-turbo')
    model_ref = MODEL_ALIASES.get(requested_model, requested_model)
    language = request.get('language')
    if language in (None, '', 'auto'):
        language = None

    explicit_compute = request.get('compute_type') or os.environ.get('YUZUKI_ASR_COMPUTE_TYPE')
    if explicit_compute:
        compute_type = str(explicit_compute)
    else:
        try:
            supported = set(ctranslate2.get_supported_compute_types(device))
        except Exception:
            supported = {'int8_float16'} if device == 'cuda' else {'int8'}
        compute_type = choose_compute_type(device, supported)

    try:
        model = WhisperModel(
            model_ref,
            device=device,
            compute_type=compute_type,
            download_root=os.environ.get('YUZUKI_ASR_MODEL_DIR') or None,
        )
    except Exception as exc:
        fail(f'cannot load {model_ref} on {device}/{compute_type}: {exc}')

    notes = [
        f'device={device} compute_type={compute_type} model={model_ref} '
        f'(requested device={requested_device}, cuda devices={cuda_devices})'
    ]
    if device_note:
        notes.append(device_note)
    if dll_dirs:
        notes.append('cuda dlls: ' + ';'.join(dll_dirs))

    prompt = request.get('prompt') or None
    no_repeat = 0
    condition = True
    if requested_model in NO_PROMPT_MODELS or model_ref in NO_PROMPT_MODELS:
        if prompt:
            notes.append('prompt ignored: anime-whisper hallucinates with an initial prompt')
        prompt = None
        no_repeat = 5
        condition = False

    # vad_filter runs Silero VAD on onnxruntime (CPU), shipped in the BASE pack.
    segments, info = model.transcribe(
        load_audio(request['media_path']),
        language=language,
        initial_prompt=prompt,
        vad_filter=True,
        no_repeat_ngram_size=no_repeat,
        condition_on_previous_text=condition,
        word_timestamps=True,
    )

    rows = []
    for segment in segments:
        text = (segment.text or '').strip()
        if not text:
            continue
        confidence = segment_confidence(segment)
        rows.append(
            {
                'start_ms': to_ms(segment.start),
                'end_ms': to_ms(segment.end),
                'text': text,
                'confidence': confidence,
            }
        )

    result = {
        'contract_version': 1,
        'adapter_id': 'faster-whisper',
        'model_id': requested_model,
        'language': getattr(info, 'language', None),
        'segments': rows,
        'notes': notes,
    }
    # Explicit UTF-8: a piped sys.stdout on Windows uses the ANSI code page.
    sys.stdout.buffer.write(json.dumps(result, ensure_ascii=False).encode("utf-8"))
    sys.stdout.buffer.flush()


main()
