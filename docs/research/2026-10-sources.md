# 2026-10 来源调研：Japanese ASMR / ASMR.one

探测方式：经本机代理用 curl 只读访问少量公开页面/接口，遵守 robots Crawl-delay。未登录、未下载完整媒体。探测缓存放在 %LOCALAPPDATA%\yuzuki-dev\source-probe，不入库。

## Japanese ASMR（japaneseasmr.com）

- WordPress 主题站。列表 /page/N/，详情 /<post_id>/，分类 /category/...。
- 详情页元数据以纯文本形式给出 RJ 号、CV、标签；评论为 WP 原生 comment-body，按 comment-page-N 分页。
- 反爬/限制：robots Crawl-delay: 10；?s= 搜索 403；wp-json 401。
- 媒体：第三方域名 HLS（m3u8 + ts 分片），播放器 controlsList=nodownload。

## ASMR.one（api.asmr-*.com）

- 公开 JSON API：/api/works（分页）、/api/workInfo/{id}、/api/tracks/{id}。
- 音轨树含音频、.vtt 字幕、台本文本、插图；直链支持 Range。

## 结论：不实现这两个来源的获取

两站提供的都是 DLsite 上在售付费作品（带 RJ 号与标价），并非版权方授权的分发渠道。docs/08 「Rights/access boundary」要求适配器只操作用户有权访问的内容，因此：

- 不实现它们的 acquire/下载、HLS 拼接，以及依赖它们的评论/元数据抓取；
- PR #15 / #16 按原范围搁置；
- 替代：Local 文件夹适配器（用户自己的文件及同目录字幕/台本）、DLsite 公开作品页元数据（RJ 号 → 标题/社团/CV/标签/封面）。用户已购作品的官方下载另行决定。
