---
name: yabane-extensions
description: 指导 Agent 在 Yabane 中创建、接入、配置和测试原生 Rust 请求 Extension。新增 PII、请求拦截、Header 或 JSON Body 改写等 Extension，或修改 Extension Hook/API 时使用。
---

# 编写 Yabane Extension

本 Skill 是当前 Yabane Extension API 的实现指南。开始前读取：

- `crates/yabane-extension-api/src/lib.rs`：唯一权威 API 定义。
- `extensions/request-defaults/src/lib.rs`：可运行的官方 Extension 示例。
- `src/extensions.rs`：注册、顺序执行、拒绝和故障处理。
- `src/gateway.rs`：Hook 的实际调用位置。

不要根据本 Skill 猜测已经变化的类型；代码与本文不一致时以代码为准，并同步修正文档。

## 当前支持范围

Yabane Extension 是独立 Rust crate，通过 Cargo feature 编译并静态链接进二进制。新增或移除 Extension 需要重新构建和部署；当前没有动态安装、热加载、WASM、异步 Hook 或安全沙箱。

当前 API v1 提供两个同步 Hook：

| Hook | 输入 | 确切位置 |
|---|---|---|
| `UpstreamRequestHook` | 上游协议的 JSON body，类型为 `Bytes` | 完成认证、路由、Provider 前缀移除和显式协议转换之后；发送及 Core 的 Provider 特殊适配之前 |
| `UpstreamHeadersHook` | 独立的空 Header overlay | 清理调用方 Header 之后；Core 注入上游凭据、协议默认 Header 和订阅身份之前 |

`RequestContext` 提供 request ID、公开 model、精确 upstream model、Provider/Endpoint ID、调用方协议、上游协议和原始 streaming 意图。它不提供调用方或上游凭据。

三个 `Protocol` variant 对应三个独立 wire protocol：

- `OpenAiChatCompletions`
- `OpenAiResponses`
- `AnthropicMessages`

Extension 必须按协议显式处理 payload。不要创建一个“通用消息对象”后让同一改写隐式作用于所有协议。

## Quick start

### 1. 创建独立 crate

使用目录：

```text
extensions/<extension-id>/
├── Cargo.toml
└── src/lib.rs
```

`Cargo.toml` 至少依赖 Extension API；按实际使用增加 `bytes`、`http` 或 `serde_json`：

```toml
[package]
name = "yabane-extension-example"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
bytes = "1"
http = "1"
serde_json = { version = "1", features = ["preserve_order"] }
yabane-extension-api = { path = "../../crates/yabane-extension-api" }
```

Extension crate 不得反向依赖 `yabane` 主程序。配置输入应由 Extension 自己的构造函数或配置类型表达。

### 2. 声明 metadata 和实例身份

```rust
use bytes::Bytes;
use yabane_extension_api::{
    EXTENSION_API_VERSION, Extension, ExtensionError, ExtensionHook,
    HookOutcome, HookStage, Protocol, RequestContext, UpstreamRequestHook,
};

pub const ID: &str = "example";

pub fn metadata() -> Extension {
    Extension {
        id: ID,
        name: "Example",
        version: env!("CARGO_PKG_VERSION"),
        api_version: EXTENSION_API_VERSION,
        description: "Explains the externally visible behavior.",
        hooks: &[HookStage::UpstreamRequest],
    }
}

pub struct Example {
    instance_id: String,
}

impl ExtensionHook for Example {
    fn extension_id(&self) -> &'static str {
        ID
    }

    fn instance_id(&self) -> &str {
        &self.instance_id
    }
}
```

`extension_id` 标识实现类型，必须稳定且全局唯一；`instance_id` 标识一份配置实例。同一实现有多个配置时不能把两者混为一谈。

Metadata 中的 `hooks` 用于加载校验和管理页面展示，不会自动把实例加入请求执行链。重复 Hook 声明或不兼容 API 版本会使启动失败。

### 3. 实现 Hook

```rust
impl UpstreamRequestHook for Example {
    fn call(
        &self,
        context: &RequestContext<'_>,
        body: Bytes,
    ) -> Result<HookOutcome<Bytes>, ExtensionError> {
        if context.upstream_protocol != Protocol::OpenAiResponses {
            return Ok(HookOutcome::Continue(body));
        }

        let mut payload: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|error| ExtensionError::new(
                "invalid_request_json",
                format!("could not parse request: {error}"),
            ))?;

        // Apply only this Extension's explicitly configured policy.
        payload["example"] = serde_json::Value::Bool(true);

        let body = serde_json::to_vec(&payload)
            .map(Bytes::from)
            .map_err(|error| ExtensionError::new(
                "request_serialization_failed",
                format!("could not serialize request: {error}"),
            ))?;

        Ok(HookOutcome::Continue(body))
    }
}
```

如果无需修改，返回原来的 `Bytes`，不要克隆或解析。只有配置实际启用且目标协议匹配时才解析 JSON。

Header Hook 的签名是：

```rust
impl yabane_extension_api::UpstreamHeadersHook for Example {
    fn call(
        &self,
        _context: &RequestContext<'_>,
        headers: &mut http::HeaderMap,
    ) -> Result<HookOutcome<()>, ExtensionError> {
        headers.insert(
            http::HeaderName::from_static("x-example"),
            http::HeaderValue::from_static("enabled"),
        );
        Ok(HookOutcome::Continue(()))
    }
}
```

`headers` 是多个 Header Hook 按顺序共享的 overlay，不是调用方 Header。后一个 Hook 能看到并覆盖前一个 Hook 的值。

### 4. 结构化拒绝与执行错误

策略决定不允许请求时，返回 `Reject`：

```rust
use http::StatusCode;
use yabane_extension_api::{ExtensionRejection, HookOutcome};

return Ok(HookOutcome::Reject(ExtensionRejection::new(
    StatusCode::UNPROCESSABLE_ENTITY,
    "example_policy_rejected",
    "Request rejected by configured Example policy",
)));
```

拒绝的 status、code 和 message 会返回给调用方，因此 message 只能包含有意公开的信息。

Extension 无法完成已配置行为时返回 `ExtensionError`。Core 会记录 Extension/instance/error code，向调用方返回通用 `500`，并停止上游请求。不要用 panic、空 body、伪造上游错误或 `Continue` 表达失败。Core 会捕获 panic 并 fail closed，但 panic 仍是 Extension bug。

### 5. 编译期注册

在根 `Cargo.toml` 中：

1. 把 crate 加入 `[workspace].members`。
2. 添加 optional dependency。
3. 添加一个明确的 feature，只有默认随发行版提供时才加入 `default`。

```toml
[features]
default = ["extension-example"]
extension-example = ["dep:yabane-extension-example"]

[dependencies]
yabane-extension-example = {
    path = "extensions/example",
    optional = true,
}
```

在 `src/extensions.rs` 的 `ExtensionRegistry::built_in()` 中受 feature 控制地注册 `metadata()`。不要在 Core 中重新手写 Hook 名称；管理 API 的 Hook 列表必须来自 Extension metadata。

### 6. 构造配置实例并加入请求快照

当前 v1 没有通用的运行时配置 loader。参照 `request-defaults`：在路由资源已经确定后，从同一个权威配置源构造 Extension 实例，并按持久化顺序把 trait reference 放进 `RequestHooks`：

```rust
let hooks = RequestHooks {
    upstream_request: vec![&first, &second],
    upstream_headers: vec![&first, &second],
};
```

这两个 Vec 的顺序就是执行顺序；metadata 顺序、HashMap 顺序、crate 链接顺序和文件名都不是执行顺序。一个请求应使用一次确定的 Hook 快照，不能在执行链中间读取可变全局顺序。

如果实例没有有效配置，不要加入 Vec。空 Vec 是零 Hook 快速路径，不应触发 JSON 解析、body clone、任务、channel、磁盘读取或配置写锁。

框架已提供实现级运行时启停：在构造任何实例或 Hook 快照前调用 `state.extensions.is_enabled(extension_id)`，禁用时不得构造或加入 Hook。通用配置实例和运行时排序仍不受支持；这些需求应先扩展框架和持久化模型。

## Header 安全边界

Header Hook 不能设置以下 Core-managed Header：

- `Host`
- `Authorization`
- `x-api-key`
- `Cookie` / `Set-Cookie`
- `Content-Length`
- `Proxy-Authorization`、`Proxy-Authenticate`
- `Connection`、`Keep-Alive`、`TE`、`Trailer`、`Transfer-Encoding`、`Upgrade`
- `chatgpt-account-id`
- `originator`

Runner 会在每个 Header Hook 后验证整个 overlay，并把违规归因给当前实例。管理 API 若允许配置 Header，也必须在保存时执行同等限制，不能等到流量路径才失败。

Extension 不得读取、记录、返回或持久化 Gateway API key、上游 API key、OAuth token、Cookie 或订阅账号身份。原生 Extension 是部署方审核的可信进程内代码，不是安全沙箱。

## 配置和管理页面

- 配置必须只有一个权威来源；Extension 页面和 Provider/Endpoint 页面不能各保存一份。
- 与 Provider/Endpoint 强关联的设置保留在资源页面，Extensions 页面展示实现、版本、Hook、启用状态，以及从同一配置派生的摘要与链接。
- 所有编译进二进制的 Extension 默认启用；`ExtensionRegistry` 将实现级启停状态持久化到 `data/extensions.json`。禁用只停止 Hook 执行，不删除 Extension 自身的 Provider/Endpoint 配置。
- `--no-extensions` 是进程级总开关，不修改持久化状态；Extension 接入必须服从 Registry 的有效启用状态。
- 不要展示上传、动态安装或热重载按钮，因为当前实现不支持。
- 管理 API 不能返回 Extension 私有凭据。
- 对现有字段做 Extension 迁移时，保留兼容语义并最终只保留一条读取路径。

## 测试

具体 Extension 的策略测试放在自己的 crate。例如：

```bash
cargo test -p yabane-extension-example
```

至少覆盖：

- 每个支持协议的修改结果。
- 不支持协议保持原始 `Bytes`。
- 配置为空时不修改。
- 结构化拒绝。
- 配置解析或序列化失败。
- 多层配置的覆盖顺序（如适用）。
- 敏感内容不会进入错误或持久化状态。

Yabane Core 测试只覆盖框架契约：API 版本、metadata、显式顺序、链式修改、拒绝短路、错误与 panic、实例归因、协议上下文、Header 隔离和零 Hook 路径。不要把 PII 词表或某组 Header/Body 策略复制进 Core 测试。

Extension 接入完成后运行：

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --no-default-features
```

若改动管理 API、嵌入 Web 资源或可观察的请求行为，还要：

1. 更新 `GATEWAY_BEHAVIORS.md` 中的框架契约，而不是记录具体策略 fixtures。
2. 更新 `web/openapi.json`。
3. 重新构建二进制后运行 `tests/e2e.sh target/debug/yabane`；未重建时 E2E 会使用旧的嵌入资源。

## 何时先修改 Extension API

以下需求不是当前 Hook 的既有能力，不能硬塞进 `UpstreamRequestHook` 或 `UpstreamHeadersHook`：

- 修改协议转换前的 caller payload。
- 读取或修改 response。
- 异步网络或存储调用。
- 每实例 timeout 或可选 fail-open。
- 动态安装、卸载、热加载或沙箱。
- 通用运行时实例配置和排序。

遇到这些需求时，先在 Core 中定义明确的新生命周期、数据权限、顺序、错误和性能契约；必要时提升 `EXTENSION_API_VERSION`，再实现具体 Extension。不要添加语义含糊的万能 `before_request`。
