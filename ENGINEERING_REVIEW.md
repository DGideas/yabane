# Yabane 工程体检与维护记录

基线：`f26f3f7` 加工作区已有改动。本文记录多轮维护窗口的检查、修复、验证边界和仍未处理的事项。所有 HTTP 复现只使用临时目录、本机模拟 Provider 和假凭据，没有读取实际 Provider 配置，也没有修改运行数据。

## 结论

不需要重写，也不需要因为约三万行实现与测试就引入数据库、前端框架或微服务。产品边界、行为契约、Rust 单元测试、真实 HTTP E2E、浏览器验证和配置事务恢复都已经具备。第一轮确认的主要风险是“功能增长快于可重复的质量门禁和故障测试”；第二轮按“安全与数据完整性 → 可重复的验证 → 生命周期与容量 → 有证据的边界整理”的顺序把其中可验证的部分逐项落地，并把不能在本轮完成的部分写成明确清单，而不是留在隐含假设里。

## 第二轮已修复

### 1. Activity 故障恢复与查询一致性

产品语义决策：**尾部截断隔离并继续服务，中间损坏拒绝启动**。理由：进程被终止只可能产生尾部残片；把残片隔离到 `*.truncated-<时间戳>` 保留证据、保留完整前缀、并以 error 级别日志告警，比让整个网关拒绝启动更符合“网关要一直可用”；而任何完整却无法解析的行、或出现在最后一行之前的坏行都不是中断写入能造成的，必须带文件名和行号拒绝启动，避免把真实损坏当成尾部截断处理。

- `append_bytes` 在失败时先 flush 再 `set_len` 回滚到写入前的长度并 `sync_data`，批次留在内存保持可查询，重试不会产生重复前缀；成功追加后也 `sync_data`。契约 **ACTIVITY-53**。
- 新增 `flushing` 在途批次，所有读路径（查询、导出、统计、保留、导入分类）按 `persisted → flushing → pending` 串联，在途追加期间的记录始终可见。契约 **ACTIVITY-41 / ACTIVITY-53**。
- 导入与计价重算改为：在内存锁内准备快照，释放锁后写盘，再用记录身份集合发布，写入期间新增的记录不被快照覆盖。契约 **ACTIVITY-53**。
- 加载路径改为读字节后按行解析：尾部残片隔离，其它损坏带 `文件 + 行号` 报错并拒绝启动。契约 **ACTIVITY-52**。
- 存储事务：回滚失败保留 journal 供下次启动重试；future 被取消时 journal 完整，下次启动把整组文件恢复到旧值。契约沿用 SAFETY 事务条目。

验证：`tests/activity-recovery.mjs`（真实二进制：残片被隔离且完整前缀逐字节保留、中间坏行以 `line N` 拒绝启动），以及 5 个新单元测试（部分追加回滚并保持可查询、在途追加可见、尾部隔离、中间损坏、完整坏尾不当截断）和 4 个并发/事务测试（导入与重算不阻塞记录和查询、恢复失败保留 journal、事务被取消后整组回滚）。故障注入按路径前缀注册并通过同一把锁判定，所以并行测试不会互相消耗注入窗口——这一点本身就是本轮发现并修掉的测试基础设施缺陷。

**未验证边界：** 注入是进程内短写/暂停，不是真实磁盘满、断电或文件系统损坏；没有做多进程同时使用同一 data 目录的运行时互斥（见“仍未处理”）。

### 2. 请求完整生命周期：客户端中途断连

- 响应体创建前先构造 `StreamCompletion`，它持有 Activity 上下文、上游 observer、usage 观察器和首字节时间；生成器正常结束、读取失败、客户端断连丢失 body 三条路径各自恰好写一条终态记录并完成 observer。契约 **PROXY-45**。
- `tests/client-disconnect.mjs`：真实 SSE 读到第一块后 abort。断言 Activity 记录为 `502`、stage `client`、category `disconnected`；Traffic Capture 的 observer 以 `interrupted` 结束并且配额从 10 回到 9（此前 capture 会停在 capturing）；随后网关继续正常服务。
- 记录写入发生在 Drop 中，因此由运行时 spawn 完成；若生成器在终态 await 之后被取消，不会写第二条记录。

### 3. 内存上限（观测不得破坏透传）

- usage 事件流解码器：`pending` 超过 1 MiB 即丢弃该行并在下一个换行后恢复解析，不再等待可能永远不出现的换行；丢弃只影响用量统计。契约 **PROXY-46**。
- 非流式跨协议转换原来用 `bytes()` 无界缓冲，现有 32 MiB 上限；聚合转换输出同样 32 MiB；单帧 8 MiB 上限保留。超限统一为 `502` + `protocol_conversion` / `response_too_large` 记录。
- 原生透传不受任何观测限制影响：`tests/response-limits.mjs` 让 Provider 返回单行 10 MiB 的事件，断言调用方收到的字节与 Provider 发出的完全一致、Activity 仍记录 `200`。
- `conversion_output_does_not_depend_on_chunk_boundaries` 用单字节切分和 300 组伪随机切分验证转换结果与网络分块无关。

### 4. 管理面硬化

- Argon2 哈希与校验移到 `spawn_blocking`；用户名不存在时用一次性构造的 dummy 哈希做同等工作量的校验，登录耗时不再区分“用户名未知”和“密码错误”。`update_profile` 不再在持锁期间占用 async worker。
- `ApiType::intern` 改为有上限的驻留表（256 个不同标识，内置类型不计入），超过上限时反序列化失败，API 输入无法再让内存永久增长。
- Management API key 的 last-use 持久化限速到每分钟一次（原来每个控制面请求都重写整个 `admin.json`）；只有写盘成功才发布内存值，契约 **SVC-37** 已补充这一点。

### 5. 优雅停机上限

- `Ctrl-C` / `SIGTERM` 仍先等待在途请求完成，但超过 `YABANE_SHUTDOWN_GRACE_SECONDS`（1–86400，默认 60 秒）后通过 shutdown notice 让在途响应体自行结束：流式与聚合转换路径都会以 `gateway` / `shutdown` 记录中断，原生响应体以正常结束（截断）而不是伪造完成；进程在 flush Activity 前最多等待 2 秒让这些记录落地。契约 **SVC-60**。
- 之前的行为是 `serve` 的 graceful shutdown 会一直等长流结束，可能让 `Ctrl-C` 挂住数分钟到数小时，而且被运行时直接丢弃的流不会留下任何记录。
- `tests/shutdown.mjs`：2 秒 grace，实测 2.03 秒退出，被中断请求出现在落盘的 day file 中，日志包含 grace 到期告警。

### 6. 质量门禁、契约与文档

- `tests/check.sh`：格式、`node --check`（全部 `.mjs`）、`bash -n`、严格 Clippy、workspace 测试、三个可选 Extension 的全部 8 种 feature 组合、重建默认二进制，然后跑 5 个进程级用例；`--full` 追加 E2E；`--load` 追加容量基线；`cargo-audit` 已安装时运行依赖审计。
- `tests/openapi-paths.mjs`：按路由和方法核对 `web/openapi.json`，发现并修正了 `/admin/providers/{id}/credentials` 的漂移（文档写成 `{provider_id}`），当前 54 条 API 路径一致。
- `tests/behavior-map.sh` 报告 254 条行为契约中有多少被测试、源码或 README 引用（当前 2/254），把“逐步建立 ID → 测试映射”变成可见指标而不是一次性任务。
- E2E 失败时 `YABANE_E2E_KEEP=1` 保留临时目录与日志。
- 新增契约条目：PROXY-45、PROXY-46、ACTIVITY-52、ACTIVITY-53、SVC-60；PROXY-38 补充“调用方中途断连”，SVC-37 补充限速语义。
- README / tests/README 补齐：Rust 1.88 与 Node 20 要求、备份与恢复（复制 `data/` 即可）、单进程 data 目录约束、停机上限与新测试入口。

## 第三轮：E2E 运行时间分析

测量范围：使用已构建的 `target/debug/yabane`，不包含编译耗时。bash 侧用 `DEBUG` trap 记录命令时间戳和行号，浏览器侧临时用 Proxy 记录 Playwright 调用耗时。基线约 103 秒，其中已登录的 `responsive-ui.mjs` 调用约 72 秒（70%）；其余约 31 秒还包含登录页、无效 session 验证和服务启动/清理，不能全部归类为 API 测试。

- Home 自动刷新检查只推进假时钟 10 秒，而页面的刷新周期是 30 秒，单个 `waitForResponse` 实测等待 18.9 秒。现在从**运行中的页面**读取并校验 `LIVE_REFRESH_INTERVAL_MS`，两处刷新检查推进完整周期；不再用正则读取可能与被测二进制版本不一致的本地源码。
- 4 个设备 × 8 个控制台视图的 `waitForTimeout(250)` 原本约 8 秒；另有约 0.5 秒用于 Help 动画中间态检查，后者保留。
- 第一版优化把视图等待改成 fetch 计数归零加两帧，这是错误的就绪判断：fetch 在响应头到达后即可 resolve，响应体、JSON 解析和渲染仍可能未完成。虽然当时 E2E 通过且降至 77–78 秒，也不能据此声称验证强度不变。该实现已移除。
- `tests/console-view-helper.mjs` 现在只在导航期间观察该视图实际触发的加载函数，等待其加载/渲染 Promise 完成，再检查视图容器、激活导航和相应 DOM。Activity 中未被主加载函数 await 的请求分页也单独等待。不会主动调用加载函数掩盖导航未触发加载的问题；未触发预期加载、Promise 拒绝、超时均失败，并恢复原函数。同步视图不额外等待网络。
- `tests/console-view-readiness.mjs` 用回环 HTTP 服务先发响应头和不完整 JSON、由测试控制何时发完响应体。在 Chrome/WebKit 上确认等待期间仍是旧 DOM，完整响应后才允许通过；另测嵌套加载、未触发加载、加载失败和超时。把旧等待实现接入同一测试会报 `accepted headers/stale DOM before the body arrived`，新实现通过。该回归已接入 `tests/e2e.sh`。

修正后的完整 E2E（包含新回归）实测 **79.45 秒，通过**。这是本机单次结果，不是跨机器性能承诺。之前插桩显示已登录浏览器调用约 45 秒，但当前版本没有重做逐项归因。Proxy 调用耗时可能重叠，调用数也包含创建 Locator 等同步操作，不能把累加值当作独占墙钟时间，或称为数千次真实浏览器交互。其余时间尚未独立分析，撤回“主要由 curl/python 进程启动造成”的结论。

**验证边界：** 新等待是显式白盒测试，依赖 `showView` 的加载函数列表及其 Promise 覆盖渲染的约定；视图加载链路变化时需要同步维护，不宣称适用于任意异步渲染。四个设备仍串行执行；进一步并行化属于测试隔离工程，需要处理共享网关配置的相互影响，本轮未实施。

## 实测容量基线

`node tests/load.mjs`（release 构建，本机回环，2 秒/档，模拟 Provider 为单线程 Node 进程）：

| 并发 | 请求数 | req/s | p50 ms | p95 ms | p99 ms | 最大 ms | 网关 RSS MiB |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 2305 | 1153 | 0.42 | 4.71 | 5.24 | 22.9 | 38.2 |
| 8 | 4914 | 2457 | 0.30 | 29.34 | 30.30 | 50.3 | 46.4 |
| 32 | 4976 | 2488 | 0.30 | 125.37 | 127.38 | 133.6 | 55.3 |

同一轮里 Activity 统计与落盘行数都等于总请求数（12245），说明高并发下记录没有丢失或重复。**这只说明基线可重复，不构成归因**：吞吐在并发 8 之后饱和、尾部延迟随并发线性上升，最可能来自单线程模拟 Provider 或 Activity 记录的串行段，需要在下一步用更真实的 Provider 和独立的观测点来区分。

## 仍未处理（按建议顺序）

1. 把 `tests/check.sh` 接入真实的仓库合并门禁。本仓库内没有版本管理的 CI 配置，不据此推断仓库外不存在 CI。
2. 行为契约 ID → 测试的映射仍是 2/254；本轮只提供了报告工具和约定，没有做批量映射。
3. 模块边界整理（`src/control.rs` 约 3.1k 行、`web/app.js` 约 4.0k 行、`src/activity.rs`、`src/gateway.rs`）尚未开始：先明确配置变更的统一锁顺序与事务边界、区分网关的路由/准备/传输/生命周期职责，再按职责拆分，并且必须在现有测试保护下进行。
4. 多进程共用同一 data 目录没有运行时互斥；目前只有“配置事务 journal + 文档约束”，进程间仍可能互相覆盖。
5. 容量工作只做到基线：Activity 全量常驻内存与统计扫描、慢磁盘、并发生成长流、大规模历史记录下的查询、协议转换路径都还没有测量；没有覆盖率数字，也没有 fuzz/性质测试（只有 chunk 切分不变性）。
6. 依赖安全审计未运行（本机没有 `cargo-audit`；`check.sh` 已条件接入）。
7. SOCKS5 只验证了复用同一个 no-redirect builder，没有线缆级重定向用例；没有用真实 OAuth/订阅账户做登录与推理验证。
8. 登录只有 `spawn_blocking` 与耗时拉平，没有速率限制或失败计数；Turnstile 仍是唯一的人机校验。
9. OpenAPI 仍是手写契约，只有路径/方法级漂移检查，没有 schema 级检查，也没有生成化。
10. E2E 的 4 个设备仍串行执行；进一步并行化前需要隔离各设备的真实配置改动，并重新测量收益与稳定性。

## 验证入口与完成标准

```bash
bash tests/check.sh          # 离线门禁：格式、语法、Clippy、8 种 feature 组合、177 个单元测试、5 个进程级用例
bash tests/check.sh --full   # 追加 API + 浏览器 + Turnstile E2E
bash tests/check.sh --load   # 追加容量基线
bash tests/behavior-map.sh   # 行为契约覆盖报告（信息性）
```

本轮结果：`cargo test --workspace`（默认 feature）197 个测试通过（177 个网关二进制 + 20 个 Extension crate），Core-only 172 个通过，8 种 feature 组合全部通过，5 个进程级用例全部通过，OpenAPI 与路由一致。`--full` 在本轮也运行过并通过（浏览器覆盖系统 Chrome 桌面/平板与 WebKit 两种手机尺寸，使用隔离 Profile；E2E 内含“无效 session 必须失败”的负向验证）。第三轮单独重跑 `tests/e2e.sh target/debug/yabane`（包含新的浏览器就绪回归）通过，耗时 79.45 秒；该数字不包含离线门禁或编译时间。

每个维护主题的完成标准保持不变：复现或失败测试 → 最小修复 → 对应行为契约 → 全部适用检查通过 → 报告未验证边界。重要资源状态、协议语义或恢复策略的变化单独评审；不把多个架构重构混成一次大改。
