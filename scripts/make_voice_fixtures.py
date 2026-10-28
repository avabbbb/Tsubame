"""Generate real-voice Japanese ASR fixtures into test-media/voice.

Each clip is <name>.wav (16 kHz mono, synthesized speech from edge-tts) plus a
same-name .srt built from edge-tts sentence boundaries. The srt is the
reference transcript: importing the folder seeds it as segments, and a real
ASR run replaces them, so compare ASR output against it.

Run with the isolated venv (edge-tts >= 7.3.1; set HTTPS_PROXY if needed):
  .yuzuki-data/tools/edge-tts-venv/Scripts/python.exe scripts/make_voice_fixtures.py
"""

import asyncio
import os
import pathlib
import subprocess

import edge_tts

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "test-media" / "voice"
CLIPS = [
    (
        "ja_nanami_weather",
        "ja-JP-NanamiNeural",
        "おはようございます。今日の東京は朝から晴れていますが、午後からは雲が増えて、夕方には雨が降るかもしれません。出かけるときは傘を忘れないでください。",
    ),
    (
        "ja_keita_station",
        "ja-JP-KeitaNeural",
        "次の電車は十時十五分に二番線から発車します。この電車は各駅に停車します。お急ぎのお客様は、向かいのホームの快速をご利用ください。",
    ),
]


async def make(name: str, voice: str, text: str) -> None:
    proxy = os.environ.get("HTTPS_PROXY") or None
    sub = edge_tts.SubMaker()
    mp3 = OUT / f"{name}.mp3"
    comm = edge_tts.Communicate(text, voice, boundary="SentenceBoundary", proxy=proxy)
    with open(mp3, "wb") as audio:
        async for chunk in comm.stream():
            if chunk["type"] == "audio":
                audio.write(chunk["data"])
            elif chunk["type"] in ("WordBoundary", "SentenceBoundary"):
                sub.feed(chunk)
    (OUT / f"{name}.srt").write_text(sub.get_srt(), encoding="utf-8")
    wav = OUT / f"{name}.wav"
    subprocess.run(
        ["ffmpeg", "-y", "-loglevel", "error", "-i", str(mp3), "-ac", "1", "-ar", "16000", str(wav)],
        check=True,
    )
    mp3.unlink()
    print(f"{wav.name}: {wav.stat().st_size} bytes")


async def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for clip in CLIPS:
        await make(*clip)


if __name__ == "__main__":
    asyncio.run(main())
