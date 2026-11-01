# 日语 ASMR → 中文同传配音：开发路径实操指引（2026-10 实测）

> 本指引基于两轮真实运行。配音链路尚未接入 Operation Registry 与 Desktop GUI（见问题日志 I-03），因此没有 GUI 截图；下文图像均由实际输出音频生成，不含素材画面、文件名或本地路径。路径一律用 <RUN_DIR>、<REPO> 占位。

## 流程总览

1. 解码：ffmpeg 生成 16 kHz 单声道 WAV（ASR）与立体声 WAV（空间分析/混音）。
2. ASR：workers/asr/faster_whisper_worker.py，stdin 传 JSON（contract_version=1、media_path、model_id=large-v3-turbo、language=ja、device=cuda）。
3. 翻译：本地网关 OpenAI/Anthropic 协议，每批 20 句，发送前经 for_cloud 脱敏，字数预算 4.5 字/秒。key 只从环境变量 YUZUKI_LLM_API_KEY 读取。
4. 空间分析：spatial.analyze.analyze_file，保存为 <RUN_DIR>/spatial/spatial_track.npz。
5. TTS：2026-10-25 实测时启动 workers/tts/edge_tts_shim.py（需 HTTPS_PROXY，edge-tts ≥ 7.3.1），先清理端口残留进程，2 并发逐句合成，超时隙则加速重合成（上限 1.5）。2026-10-29 起产品路径为安装 runtime.edge-tts 包并选择 Edge TTS (online) provider，经 yuzuki tts synthesize 驱动，不再需要 shim 与系统 ffmpeg。
6. 渲染 + 混音 + 导出：python <REPO>/workers/dubber/dev_render_mix.py <RUN_DIR> <REPO> source_trajectory。

## 耗时对比

![各阶段耗时：两轮对比](../media/dub-run-2026-10/stage_timings.svg)

| 阶段 | 第一轮 | 第二轮 |
|---|---|---|
| ASR | 57.5 s（260 句） | 338.4 s（264 句，见 I-12） |
| 翻译 | 342.8 s | 290.0 s |
| 空间分析 | 29.7 s | 26.8 s |
| TTS | 675.7 s | 627.5 s |
| 渲染+混音+封装 | 229.0 s | 224.9 s |
| 显存峰值（ASR） | 3218 MiB | 3389 MiB |

## 波形：原声 / 双语混音 / 配音轨

从上到下：原声（蓝）、双语混音（橙）、空间化后的中文配音轨（绿），各分左右声道，截取 60 s 片段。配音出现时原声被压低 9 dB。

![波形对比](../media/dub-run-2026-10/waveform_compare.png)

## 空间一致性

每点一句：横轴为原声该句的左右响度差（ILD），纵轴为渲染后中文配音的 ILD。第一轮相关系数 0.973，平均差 1.22 dB。

![逐句 ILD 对比](../media/dub-run-2026-10/ild_scatter.svg)

## 配音轨频谱

![配音轨频谱](../media/dub-run-2026-10/dub_spectrum.png)

可见配音能量集中在约 11 kHz 以下：edge-tts 输出 24 kHz 采样，重采样到 44.1 kHz 后高频为空，与原声的高频气声存在音色差。波形图为 sqrt 刻度以便看清低音量 ASMR。

## 动图与短视频

双语混音 12 s 波形动图；短视频为 20 s 波形动画，不含音频（素材版权归原作者，仓库不收录音频片段）。

![双语混音波形动图](../media/dub-run-2026-10/mix_waveform.gif)

[波形短视频（无音频）](../media/dub-run-2026-10/mix_waveform_silent.mp4)

## 成品规格

- mp4：视频直接复制；音轨 1 为中文配音 + 压低的日语原声（默认，chi），音轨 2 为未改动的日语原声（jpn）。
- 响度：约 -25 LUFS，真峰值 -1.0 至 -1.1 dBTP。
- 尚未做人工听感抽查。

## 常见坑

- 时长读成约 89478 s：shim 未修正管道 WAV 头（I-09）。
- 改了 shim 代码却没生效：旧子进程仍占端口（I-10）。
- 偶发 502：代理连接超时，靠重试（I-11）。
- 网关 401：key 未进环境变量（I-08）。
