# 协议转换专项检查（2026-09-21）

范围：Chat Completions、Responses、Anthropic Messages 的请求、JSON 响应、SSE、聚合响应及工具多轮重放。基线为 `497f22c` 加本次会话已完成的空 delta 修复；没有更改运行实例配置、修复用户会话文件、提交或部署。

## 结论

问题不是一种模型不稳定，而是适配器对**完整回合、内容生命周期和协议边界**的表达不足。已有测试能证明字段映射，却遗漏了“转换输出 → 客户端组装 → 加工具结果 → 下一轮请求”的路径。有的旧测试还固定了错误行为。

本轮已修复以下可验证问题；契约见 `GATEWAY_BEHAVIORS.md` 的 PROXY-52–63。

| 问题 | 用户可见后果 | 修复/验证 |
| --- | --- | --- |
| 空 Chat 文本 delta 创建独立 Responses message | 下一轮工具调用与结果之间插入空 assistant，400 | 不创建空文本项；流式/聚合与多轮 HTTP 重放测试 |
| function_call 之间/之后的 assistant 文本拆开工具批次 | 即使非空文本也会触发 missing tool results | 文本保留在同一个 Chat assistant 工具回合中；顺序调用仍分回合；严格模拟 Provider 校验 |
| Chat assistant 带 tools 时绕过 content 转换 | 原样发出 Chat `text` 块，或 assistant 使用错误的 Responses part 类型 | 两条分支统一转换；assistant `output_text`、其他角色 `input_text` |
| Anthropic thinking 签名当作 OpenAI encrypted_content | 无法解密/重放的外来状态被送到 Provider | 通用转换省略不可移植 thinking，不变成可见文本；原生透传不变 |
| previous_response_id 被删除，conversation 泄漏到其他协议 | 请求可能成功但历史被截断，或收到无意义的 Provider 400 | 跨协议本地 400，提示提交完整历史；不回显状态 ID；原生不变 |
| JSON 工具参数先解析再序列化 | 大整数被舍入、数字形式/空白改变，甚至 JSON 字符串被解包 | 两种 OpenAI 协议间保留参数字符串；必须转 object 时使用 arbitrary_precision，保留数字精度 |
| Anthropic 块索引直接用源索引 + 1；Chat 工具也泄漏稀疏索引 | SDK 按数组拼装时访问不存在的项 | 独立连续索引、稳定 delta/stop 引用，聚合块顺序与 SSE 一致 |
| 不读起始/结束事件携带的内容 | 丢初始文本、丢工具参数，错误地变成空调用 | 读取 Anthropic block start、Responses 初始参数及累计快照；只补经前缀校验的缺失后缀；不同内容/身份报错 |
| 只有终止标记也宣告成功 | 中断或非完整回复被包装成空成功 | 要求生成 finish；queued/in_progress/cancelled 的 Responses JSON 不能变成已完成 Chat/Anthropic |
| stop/parallel/max_tokens 等形状不匹配 | Anthropic 参数校验失败或并行限制失效 | 标量 stop → 数组、parallel 布尔互转、移除无目标表示的 reasoning_effort、清理重复 token 上限 |
| 文档在 Anthropic → Chat 消失，inline 文件丢 MIME | 模型收不到附件，或 Provider 拒绝 file_data | 文档/工具结果映射 file；保留 MIME data URI；无法识别 MIME 时本地报错而非猜测/丢弃 |
| refusal 块/字段被忽略 | 有拒答理由却返回空的成功答案 | OpenAI 输出保留 refusal 字段和生命周期；Anthropic 用可见文本保留理由；跨协议历史重放也保留理由 |

额外防线：工具 delta 无 start、同索引工具身份改变、结束快照与已发内容不一致，均失败而非生成匿名/不同调用。前缀校验使用长度与 SHA-256 摘要，流式路径不为此另存完整内容；每个转换流的文本/工具追踪各限 4096 项，防止空事件无限扩展状态。

### 旧测试与契约漂移

- 原 `anthropic_to_responses_preserves_interleaved_block_order` 把 foreign reasoning 签名输出为 OpenAI 加密状态当成正确行为，已改成保留普通内容顺序并省略不可移植状态。
- 原文档测试要求 `file_data` 是不带 MIME 的裸 base64，已改为带 MIME 的 data URI，并新增双向 round-trip 与真实 HTTP 检查。
- 一个 SSE fixture 把完整工具参数同时放在 added 和 delta 中，等价重复发了两次参数；已修正 fixture，而非令实现忽略真实初始内容。
- `ENGINEERING_REVIEW.md` 是历史维护记录，其中 32 MiB 限额和旧测试计数不能代表当前实现。当前缓冲体上限是 128 MiB、单帧上限 8 MiB，以行为契约和源码为准。

## 与 Bifrost 的对照

只读参考 `~/3rd/bifrost`，当时 HEAD 为 `ece3ae0ca`，未修改该仓库：

- `core/schemas/mux.go`：工具调用聚合、内容/工具表示、refusal 映射。注意它的通用 `ToChatMessages` 也在遇到非 function_call 时 flush，不能据“成熟实现”假定它已覆盖 Yabane 这次的文本穿插情况。
- `core/providers/anthropic/responses.go`：独立 block index 分配、block type 校验、累计内容与已发前缀校验；这些是本轮借鉴的状态管理原则。
- `core/providers/openai/responses.go`：外来 reasoning 签名与 OpenAI reasoning 的不同、图片 detail 的处理。没有照搬其基于模型能力表/名称推断的行为；Yabane 继续遵循显式配置、不猜能力的边界。

没有用另一项目的输出当作唯一正确性标准：本轮以目标协议形状、严格配对检查、SDK 式组装、逐字节参数与多轮重放作为断言。

## 验证结果

- `bash tests/check.sh --full` **通过**：格式/语法、严格 Clippy、workspace 测试、8 种 Extension feature 组合、真实进程 HTTP 用例、完整认证/路由与浏览器 E2E。
- 默认网关测试 **230 个通过**；相对本轮开始时的空 delta 修复后基线，新增 **18 个 Rust 回归测试**，并扩展两个 HTTP 测试套件。
- 浏览器：系统 Chrome 的桌面/平板、Playwright WebKit 的两种手机尺寸，隔离 Profile；已登录页面及单独的退出登录页面均检查。
- `cargo build --release --locked` **通过**；用新 release 二进制再跑 `reasoning-conversion.mjs`、`stream-semantics.mjs` **通过**。
- 新的协议 HTTP 测试只用临时目录、回环模拟 Provider 和假凭据。没有对真实供应商做全协议/全模型在线验收，也没有调用实际账户做 reasoning 或上传文件的跨 Provider 验证。
- `cargo audit` 未执行：本机未安装 cargo-audit。没有性能/负载或 fuzz 覆盖结论。

主要新增回归：`src/protocol/regression_tests.rs`、`src/protocol_stream/regression_tests.rs`；实际请求与客户端重放：`tests/reasoning-conversion.mjs`、`tests/stream-semantics.mjs`。

## 仍需明确的兼容性边界

这不是“三协议完全无损互换”的承诺：

1. **同协议的跨 Provider 状态并不自动可移植。** 原生 Responses 仍透明转发 reasoning、item ID、历史消息；已有会话里的坏空消息若直接走原生 Responses，不会被网关偷偷修复。PROXY-53 只在实际跨协议转换时分组，不修改用户的会话文件。
2. **通用转换有损。** reasoning、Provider 原生工具及无目标等价字段不保证保留；thinking 模式若要求专有 reasoning replay 字段，需要单独的显式 Provider 适配与线上验证，不能仅靠三协议 JSON 互转。
3. **文件 ID 只转换形状，不搬迁文件。** `file_id`、URL 可访问性和工具结果中多模态内容的接受能力由实际 Provider 决定，模拟测试不能代替供应商验收。
4. **没有实现服务端会话存储。** `previous_response_id`/`conversation` 跨协议拒绝是有意边界，不会通过新数据库或自动历史抓取把范围扩大。
5. **发布尚未执行。** 工作区和 `target/release/yabane` 已更新；`~/bin/yabane/yabane` 及其常驻进程仍是旧版本。需要单独部署/重启后，线上请求才会受益。

对于高频工具调用的加权路由，仍建议优先让所有目的地接受同一种原生协议。转换是明确受测的兼容层，不应被当作 Provider 私有状态的迁移层。
