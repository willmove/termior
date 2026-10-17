# Codex `item/permissions/requestApproval` 夹具

## 来源

构建样本时环境里没有 `codex` 可执行文件，所以这里不是一次真实 app-server 会话录制。请求按 OpenAI Codex 两个已核验 tag 的生成 schema 和序列化规则重建：

- [`rust-v0.137.0`](https://github.com/openai/codex/tree/rust-v0.137.0/codex-rs/app-server-protocol)
- [`rust-v0.153.0`](https://github.com/openai/codex/tree/rust-v0.153.0/codex-rs/app-server-protocol)

这两条 minor 是 `CodexProtocol::stable_verified()` 接受的版本线。下列生成物在两个 tag 上的 JSON 字段一致（路径新类型落到线上都是字符串）：

- `schema/typescript/v2/PermissionsRequestApprovalParams.ts`
- `schema/typescript/v2/PermissionsRequestApprovalResponse.ts`
- `schema/typescript/v2/RequestPermissionProfile.ts`
- `schema/typescript/v2/GrantedPermissionProfile.ts`
- `schema/typescript/v2/AdditionalFileSystemPermissions.ts`
- `schema/typescript/v2/AdditionalNetworkPermissions.ts`
- `schema/typescript/v2/FileSystemSandboxEntry.ts`
- `schema/typescript/v2/FileSystemPath.ts`
- `schema/typescript/v2/FileSystemAccessMode.ts`
- `schema/typescript/v2/FileSystemSpecialPath.ts`
- `schema/typescript/v2/PermissionGrantScope.ts`

对应源码是 `src/protocol/v2/permissions.rs`。`PermissionsRequestApprovalParams` 含 `threadId`、`turnId`、`itemId`、`environmentId`、`startedAtMs`、`cwd`、`reason`、`permissions`。`RequestPermissionProfile` 只含 `network` 与 `fileSystem`。`FileSystemAccessMode` 为 `read` | `write` | `deny`。`FileSystemPath` 用 `type` 区分 `path`、`glob_pattern`、`special`。

`From<CoreFileSystemPermissions>` 在这两个 tag 上都会把 legacy `read` / `write` 根同时写成 `entries`。`entries` 带 `skip_serializing_if`，因此只含 `read` / `write` 的载荷仍然能通过 schema。`environmentId` 与 `reason` 是没有 `skip_serializing_if` 的 `Option`，空值序列化成 `null`。`read` / `write` 同样总是出现。`globScanMaxDepth` 与 `entries` 在 `None` 时省略。

`PermissionGrantScope` 在响应上，取值 `turn` | `session`。请求参数没有 `scope`。`strictAutoReview` 可选；Termior 的响应不带这个字段，服务端把缺省当成 `false`。

夹具是 `serde_json` 紧凑编码经过脱敏后的结果。字段集合与 schema 一致。工作区里其它 crate 会打开 `serde_json` 的 `preserve_order`，整仓测试时对象键顺序和单独编译本 crate 不同。契约测试比较解析后的 JSON 值。

## 三条请求

| id | 形状 |
| --- | --- |
| 61 | legacy `read` / `write` 与对应 `entries` 一起出现，`network.enabled = true`，`reason` 里带一段 `sk-` 令牌 |
| 62 | `glob_pattern` deny 与 `project_roots` special；`read`、`write`、`network` 为 `null` |
| 63 | 省略 `entries` 的 `read` / `write` 根，其中 `write` 有两条，便于子集回放 |

`startedAtMs` 使用协议测试里的 `1`。线程与 turn 使用 `thr_fixture` / `turn_fixture`。62 的 `environmentId` 使用官方示例里的 `local`。

## 脱敏

预像只存在于 `tests/permissions_fixture.rs`。家目录前缀作为 `termior_store::StreamingRedactor` 的已知 secret；`reason` 里的 `sk-` 令牌由 redactor 的前缀规则替换。提交的 `requests.jsonl` 是 redactor 的输出，替换标记为 `[REDACTED]`。样本里不保留用户名、家目录或令牌原文。契约测试会再跑一遍 redactor，并拒绝这些子串重新出现。

## 响应

`responses.jsonl` 按 `id` + `decision` 与请求配对。`message` 是 Termior 当前审批映射写到 stdio 的 JSON-RPC 结果：`result.permissions` 为勾选子集，`result.scope` 固定为 `turn`。`granted` 是审批卡上仍勾选的 entry id。

- `approve-all`：默认全选，回传整份 profile
- `partial`：只回传列出的子集
- `deny`：回传 `{permissions: {}, scope: "turn"}`

现有映射把非数组 JSON 值当成一条权限。因此 62 里值为 `null` 的 `network`、`fileSystem.read`、`fileSystem.write` 各有一个勾选框；全部批准时这些 `null` 会原样出现在响应里。部分批准未勾选的键会被丢掉。

请求 schema 没有 scope 字段。服务端若另带未知 `scope`，仍由 `codex.rs` 与 `composer_view.rs` 里的合成用例覆盖：卡片显示该值，授权范围仍是 `turn`。

## 回放

`fake_app_server.rs` 只依赖标准库。契约测试用 `rustc` 把它编成可执行文件，供 `CodexBackend` 按真实方式启动：`--version` 打印 `codex-cli 0.153.0`，`app-server --stdio` 完成握手后吐出一条请求，并把客户端响应原样写到旁边的 `captured-response.json`。

```bash
cargo test -p termior-agent-host --test permissions_fixture
cargo test -p termior-ui --lib recorded_permission_fixtures_map_through_the_approval_card
```
