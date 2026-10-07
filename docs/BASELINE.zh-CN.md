# Tsubame 初始产品与架构基线

## 一句话定位

**Tsubame 是一个 Local-first 的 ASMR 媒体库、播放器与 AI 配音工作台。**

它不是把“下载器 / 播放器 / ASMR-Dubber”做成三个页面，而是让一个作品从发现、下载、播放、字幕、逐句编辑、翻译、配音到导出始终处于同一个本地工作台里。

## UI

采用 Codex / VS Code 类 Workbench：

```text
┌────┬──────────────┬──────────────────────────────────────┬──────────────────┐
│    │              │ Tabs                                 │                  │
│Rail│ Context      ├──────────────────────────────────────┤ Inspector        │
│    │ Sidebar      │          Main Workspace              │                  │
├────┴──────────────┴──────────────────────────────────────┴──────────────────┤
│ Global Player / Job Status Bar                                               │
└───────────────────────────────────────────────────────────────────────────────┘
```

视觉目标：Apple Music × Codex Workbench。布局保持专业工作台，媒体视觉更强调封面、播放器质感与轻量材质。

## 最重要的数据对象：Segment

字幕不是一个 SRT 文本文件，而是一组可独立编辑的句子对象。

每一句都可以拥有：

- start/end 时间；
- 原文；
- 译文；
- ASR / 精修 / 翻译来源；
- 生成语音；
- revision；
- dirty dependency。

因此修改一句译文时，只需要让该句 TTS / Mix 失效，不重新跑整段 ASR。

## 模型架构

不允许“字幕功能 = Whisper”。

统一按照：

```text
Feature
  ↓
Capability
  ↓
Processing Profile
  ↓
Provider
  ↓
Model
  ↓
Runtime / Transport
```

ASR 可以来自 Whisper.cpp、Faster-Whisper、SenseVoice、Apple Speech、阿里云模型或未来的新模型。

翻译、精修、TTS 同理。

### BYOK

远端 Provider 使用用户自己的 Key。支持的 Provider 应尽可能自动读取当前 Key 可用的模型列表，再结合 Tsubame 的 capability catalog 判断模型能力。

Key 不存 SQLite，只存系统凭据管理器，数据库只保存 `secret_ref`。

### 本地与远端 Provider

**本地和远端不是两套模型体系。**

统一抽象为：

```text
Capability
  ↓
Provider
  ↓
Model
  ↓
Adapter
  ↓
Runtime / Transport
```

Provider 的执行方式可以是：

- `remote_api`：OpenAI、阿里等付费/BYOK API；
- `local_server`：LM Studio、Ollama、llama.cpp server 等 localhost/LAN 服务；
- `local_runtime`：Faster-Whisper、SenseVoice、whisper.cpp 等 Tsubame 本地 Runtime；
- `native_os`：Apple SpeechAnalyzer 等系统原生能力。

因此 ASR、翻译、精修、TTS 都可以在同一个能力选择器中同时出现“本地模型”和“远端 API”。功能代码不能写 `if local` / `if online` 两套逻辑，而是解析满足 Capability 的 Provider + Model。


## Desktop 与模型安装

普通用户只安装 Tsubame Desktop。

不要让用户自己安装：

- Python；
- Node；
- Rust；
- FFmpeg；
- CUDA 开发工具链。

大型 Runtime / Model 由 Desktop 按 Processing Profile 按需下载，经过哈希验证后安装到 Tsubame 自己的目录。

## Skill 与本地 Agent

仓库提供 `$tsubame` Skill。

它负责：

1. 检测 Desktop；
2. 必要时 bootstrap；
3. `doctor` 检测硬件和运行时；
4. 解析 Processing Profile；
5. 规划缺少的 Runtime / Model；
6. 让 Tsubame Desktop 自己完成安装；
7. 通过 CLI / MCP / local RPC 操作 Desktop。

Agent 不通过屏幕点击来控制 Tsubame。

## 下载源

下载与发现做成 `SourceAdapter`：

- Local；
- ASMR.one；
- Japanese ASMR；
- 未来其它来源。

站点解析逻辑不能进入 UI 或核心 Domain。

## 实现与参考边界

Tsubame 的正式 Desktop 技术栈：

```text
React + TypeScript
       ↓
    Tauri 2
       ↓
      Rust
       ↓
SQLite / 本地文件 / Job / Runtime Manager
```

主项目采用 **MIT**。

- ASPlayer：只参考播放、字幕时间轴等产品/交互设计，不复制、移植或链接其 GPL 源码。
- ASMR-Dubber：MIT，可在后续 Worker 层按许可要求选择性复用处理能力。
- asmr-downloader：MIT，主要参考下载队列、重试、同步等设计，必要时可选择性复用兼容代码。

## 名称与声优

项目名 **Tsubame** 是因为作者喜欢声优 **柚木つばめ（Yuzuki Tsubame）**，属于个人致敬命名。

项目与她本人没有官方关联，也不会因为这个名字而内置她的声音、模型、录音或形象素材。

她的官方站明确声明禁止将其声音用于 AI 学习/使用，所以 Tsubame 的 Voice Clone 功能不能把她作为内置/默认/示例声线。

## 开发优先级

1. 文档/架构基线；
2. Desktop 原型与 Workbench；
3. React + Tauri/Rust + MIT clean-room reset；
4. Canonical Segment Editor；
5. Provider / Model Registry；
6. 模块化 ASR；
7. Transcript Refine / Alignment；
8. ASMR-Dubber Worker；
9. 单句 Regenerate；
10. Runtime Bootstrap；
11. Skill + CLI/MCP；
12. Source Registry + 下载；
13. Japanese ASMR Adapter；
14. Apple Music 风格媒体库完善；
15. Windows 安装包与 Release。
