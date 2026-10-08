# Zeta — Rust 多模态 Agent Harness 脚手架

把 Claude Code / OpenCode / ZCode / Codex / DeepSeek harness 的优点集于一体的 Rust 脚手架：**低延迟、prefix-cache 友好、防循环、可 24h 无人值守、支持 RSI 自升级**。适配 GLM-5.3-Flash 等 OpenAI 兼容协议后端（vLLM 直连即可）。

## 特性

| 能力 | 说明 |
|---|---|
| 多模态 | 文本 / 图片（data-url vision）/ 语音（input_audio），图预算硬顶 4 张防 `[image evicted]` 死循环 |
| 防无限循环 | LoopGuard：相同调用硬顶 + 滑动窗口周期检测(A,B,A,B…) + Warn 先行、Deny 兜底（学 zcode 熔断教训：软手段优先） |
| 验收机制 | `submit_acceptance` 工具：完成声明必须带 verify 命令，harness 真实重放，exit 0 才算 PASS |
| 24h 运行 | append-only JSONL journal（fsync）+ 崩溃恢复（dangling tool_calls 修补）+ watchdog 指数退避 + daemon 模式任务失败不退出 |
| 低延迟/高 cache | system+tools 固定在头部、历史只追加；压缩只动中段（头尾不动保 prefix cache）；`prompt_cache_hit_tokens` 透传统计 |
| 记忆 | BM25（中英混合分词）+ JSONL 持久化，任务开工自动召回注入 |
| TUI | ratatui opencode 风格：事件流 + 状态栏（步数/cache%/tok/s/prompt长度/compact/子代理/定时任务/记忆/错误/温度/内存） |
| RSI 自升级 | 只许改本仓库 → fmt/test/build 三道门全过才算数 → 审计落 memory；防路径逃逸 |
| npm 分发 | `npm install -g zeta-harness` 自动下载平台预编译二进制 |

## 安装

```bash
# 方式一：npm（自动拉预编译二进制）
npm install -g zeta-harness

# 方式二：源码
cargo install --git https://github.com/zhangjianbang-nb/zeta
```

## 快速开始

```bash
# 配置（~/.nexus/config.toml，首次运行自动生成）
zeta run "分析当前目录的代码结构，输出报告到 report.md"

# opencode 风格 TUI
zeta tui

# 24h 无人值守（tasks.txt 每行一个任务，# 注释）
zeta daemon tasks.txt

# 查记忆
zeta memory "上次教训"
```

## 架构

```
crates/
  nexus-core      消息/会话/journal(fsync)/CachePolicy(保前缀压缩)/Config
  nexus-guard     LoopGuard(防循环+图预算) / Watchdog(指数退避+GiveUp)
  nexus-memory    MemoryStore(JSONL) + Bm25(中文按字/英文按词)
  nexus-provider  OpenAI 兼容 SSE 客户端（GLM-5.3-Flash/vLLM 实测协议，cache usage 透传）
  nexus-tools     bash(超时+截断) / fs(read/write/edit/list) / image(info/view) / audio
  nexus-agent     主循环 / submit_acceptance 验收 / RsiController / Metrics
  nexus-tui       ratatui TUI（事件流+全指标状态栏）
  nexus-cli       tui/run/daemon/rsi/memory 子命令
```

## 针对 zcode 历史问题的对策

- **图数量死循环**（[image evicted] 重读）：图预算硬顶 + `image_info` 无视觉成本探测 + Deny 后给模型结构化提示改道。
- **think 循环**（周期型回路）：滑动窗口周期 2-4 检测，先 Warn 注入提醒、连续 Warn 才 Deny。
- **工具无限循环**：同参数调用计数硬顶（默认 3 次）。
- **崩溃**：journal fsync + 重启 `repair_dangling_tool_calls` 补中断的 tool result，会话无缝续跑。
- **上下文膨胀**：CachePolicy 保前缀压缩（头尾不动，cache 不断）。

## License

MIT
