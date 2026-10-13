# Agent Tree 软链接（分身）设计

状态：设计审查，未实现。基线为 `139efa6`；本次只增加此文档，不改代码、Policy 默认、
数据库或运行中的工作负载。目标是把常用 App 和跨团队 Agent 放到使用者附近，降低发现与
寻址成本，而不是复制进程、身份或权限。

## 建议与需要质疑的假设

采用“文件夹内有名字的别名节点 → 同 workspace 的稳定 Agent/App ID”。v1 不链接
文件夹、不链接链接、不跨 workspace、不复制 credential，也不创建新 workload。
真实目标只有一份规范 placement；多少个别名都不能改变它。

leader 的发现与授权分离、规范身份、删除后 broken 的方向成立，但需要补四个约束：

1. 只链接 Agent/App 仍不足以自动排除循环：创建接口必须禁止 link ID 成为 target，
   不能保存另一个别名的路径。用户以别名创建链接时先解析到真实目标并保存其 ID。
2. 链接不授予权限，也就不能自动解决跨团队 App 的“同文件夹才可用”。必须允许显示
   “已找到，但未获授权”，另设计明确授权；不能将别名 placement 当身份 placement。
3. 规范路径是可变的展示信息，不是规范身份。授权、投递、审计关联以稳定 ID 为准；
   Message 路径保留发送时快照，不能随着目标移动回写历史。
4. 现有名称校验没有统一检查 folder 与 workload 的冲突。要求所有节点同目录唯一，
   需统一 reservation，并报告历史冲突；不能只给新 links 加一条唯一索引就称已解决。

建议 v1 对目标执行非破坏性操作自动跟随；节点编辑永不跟随；目标生命周期/管理写入
经过别名时要求显式 `--follow-link`。这是比全命令无条件跟随稍保守的设计，防止用户
将“删除别名”误解成“删除远端团队的服务”。具体命令矩阵见下文。

## 已核实的现有边界

- [树模型](../../crates/treer-protocol/src/workload_tree.rs) 目前只有 folders、placements；
  placement 用 `agent_id`，每个真实身份一份。Managed App 还涉及 deployment `app_id`、
  当前 `runtime_agent_id`、稳定 `service_id`，这三者不能混用。
- [路径解析](../../crates/treer-protocol/src/targets.rs) 与
  [现有设计](2026-10-12-agent-path-targets.md)：Agent 从自己的规范所在目录解析相对路径，
  operator 从根目录解析；裸名字只匹配兄弟节点，无全局名称回退。
- [持久树操作](../../crates/treer-proxy/src/auth/workload_tree.rs) 锁 workspace 数据库行，
  但 folder 与 Agent/App 各有自己的冲突检查。当前删除非空 folder 可提升子节点；
  CLI `rmdir` 要求空目录。
- [树授权](../../crates/treer-proxy/src/api/workload_tree.rs) 当前是 workspace 级
  `agent.tree.read/manage`，不是已经存在的 folder 级细粒度 ACL。
- [CLI ls](../../crates/treer-agent-server/src/cli_workload_tree.rs) 返回 JSON
  `{path, folders, agents}`。不能为了显示箭头把 JSON 当纯文本输出，也不能将别名
  塞进 `agents` 数组伪造一个新身份。
- [email-endpoint](../../apps/email-endpoint/email_endpoint.py) 的 `placement()` 由真实
  `agent_id` 查规范节点，要求非 App Agent 与 endpoint 同 `folder_id`；endpoint 移动
  后 activation 需再次确认。此规则是 App 自己的授权，不是 Core Policy grant。

## 数据模型、Postgres 和 NATS

新增 `workload_tree_links`，与 folders/placements 并存，不把链接建成 Agent credential。
建议字段：

| 字段 | 含义 |
| --- | --- |
| `workspace_id`, `link_id` | workspace 作用域；`link_<uuid>` 永不复用 |
| `folder_id`, `name`, `position` | 节点位置；root 用 NULL，排序与真实 workload 混合 |
| `target_kind`, `target_id` | `agent` + Agent ID，或 `app` + deployment App ID |
| `revision` | 链接修改 CAS 版本；创建为 1 |
| `created_by_kind`, `created_by_id`, `created_at`, `updated_at` | 由认证上下文填写，非客户端自报 |

App 链接存稳定 deployment `app_id`，按操作映射到当前 runtime/service。不能存易变
runtime PID、run_generation 或仅凭名称猜 service。非 Managed App 的独立 network
service 暂不支持；未来需新增明确 target kind，不能暗中视为 App。

`target_path`、live/offline/broken 和 target display name 均由查询派生，不作路由依据。
没有到 target 的级联删除 FK，否则无法保留 broken 节点；workspace/folder 引用仍用
同 workspace 复合约束。创建时在事务内核实 target 存在且所属 workspace，目标删除
路径与创建路径共用 Tree lock，保证提交时可线性排序。离线/停止不等于 broken。
真实删除留下 target ID；同路径新建一个不同 ID 的对象不得让旧链接复活。

v1 不支持原地 retarget；换目标必须删旧链接、创建新 link ID。rename/move 可以保留
link ID 并增加 revision，降低“昨天指向 reviewer，今天指向管理员”的竞态和欺骗风险。

所有 node 创建、改名、move 和 folder 提升子节点统一使用
`reserve_tree_label(workspace, parent, name, excluding_node)`，在现有 workspace 行锁内
同时检查 folder/workload/link，保持现有大小写敏感规则。新增标签沿用可见路径组件校验，
并保留 `link_`、既有 ID/self/dot 名称。root 的 NULL 不能靠普通 UNIQUE 约束保障；
links 自身可用 root/非 root 部分唯一索引，跨表统一约束依赖集中事务入口及回归测试。
批量提升必须先检查整个最终集合，再写入；冲突则整笔失败，不覆盖或自动改名。

迁移前只读检测历史 folder/workload 同名，输出给 operator 的有权限冲突清单。
不自动重命名。冲突节点仍可用稳定 ID；受影响路径报 `tree_name_conflict`，禁止选择
第一项或通过链接优先级掩盖冲突。后续可评估统一 node registry 的数据库唯一约束，
但不在 links v1 顺手搬迁所有身份表。

Postgres 是唯一 durable source of truth。增加 workspace `tree_revision`，真实变化才
递增；link、目标改名/移动/删除都参与。树快照在同一事务读取，避免 ID/name/placement
来自不同时间。NATS 只广播 workspace/revision/变更 node ID 的失效通知，不保存第二份
链接权威状态，不包含凭据或不可见目标的名称。推荐事务 outbox 发布；订阅断线或 revision
缺口重新 GET 权威快照，不能以 NATS 缓存单独完成授权/dispatch。已有不可靠事件投递不应
被宣传为恰好一次；首次版本允许 bounded refresh 兜底，但必须明确缓存失效上限。

## 路径与命令语义

保持绝对/相对、`self`、裸兄弟名与 `..` 的现有规则。链接只能是最终路径组件；
`/team/tool/child` 若 tool 为 link 返回 `not_a_directory`。`ls -R` 列出链接一次，
不遍历它的目标；folder resolver 不跟随链接。相对 cwd 永远是调用者的规范 placement，
不能因为它通过某个别名收到 prompt 而切换到别名文件夹。

拆分 `resolve_node`（不跟随）与 `resolve_workload`（允许单跳），返回 typed resolution：
node ID/kind/path/revision、target kind/ID、规范目标路径、tree revision。ID 优先规则
包含 link ID，但既有 Agent/App ID 的行为不变。没有全局 alias-name 搜索回退。

| 命令或操作 | 链接行为 |
| --- | --- |
| `agent prompt`, queue read/cancel, output/transcript/show | 自动跟随真实目标，逐项按现有 action 授权；App 不支持 prompt/AIS 就明确拒绝 |
| `message send --to <link>` | 自动跟随真实 Agent mailbox；App 只有已支持的 App/Core Message recipient 才接受，否则 `target_kind_unsupported` |
| `treer exec --machine ...`, SSH 的 machine 参数 | 不接受 link；机器命名空间保持独立，不由 Agent 链接推导 machine.exec 权限 |
| 将来若有 Agent-target exec | 可解析 target，但评估真实 machine/Agent action；不能把 alias 当 cwd 或 OS 路径 |
| `app show`、打开 App UI、查询已有 service 地址 | App link 映射稳定 deployment/service；真实 Agent link 不冒充 App，不创建新公网域名 |
| `token create`、网络 App 访问 | 若增加 Tree App 参数，显式解析到真实 service audience，重新执行 token/network 授权；不是用别名作 JWT audience 或 Host |
| Agent/App start/stop/restart，admin startup set/rename/delete，将来的 app update | 经链接默认报 `link_target_confirmation_required` 并显示有权限的规范目标；`--follow-link` 后正常授权/执行，不自动额外授予权限 |
| admin startup show、只读管理查询 | 可跟随，但仍要求 startup.read 等，不因“只读”返回命令中的秘密 |
| `agent ln <target> <link-path>` | target 可经已有 link 解析，但只存最终真实 ID；destination 按创建节点处理，存在则失败，无 v1 `-f` 覆盖 |
| `agent readlink <link>` | 返回链接元数据和有权限可见的规范路径；不暴露 credential，不读取文件系统 |
| `agent rm <link>` | 只删链接，不跟随，即使 broken 也可删；v1 rm 非 link 报错，真实删除保留现有 admin delete/app delete |
| `agent mv <link> <destination>` | 只移动/改名 link；不修改真实 placement/name，destination link 不是目录，也不能覆盖 |
| `agent rmdir`/folder 删除 | link 计作子节点；空目录删除拒绝。提升子节点模式提升 link 位置但不移动真实目标，统一检查冲突 |
| `agent ls` / `ls -R` | 显示 `名字 -> 目标规范路径`；删除目标标 `broken`，不递归跟随 |

示例（拟议语法，不代表已发布）：

```sh
treer agent ln /shared/issues /research/issues
treer agent ln /review-team/reviewer /research/reviewer
treer agent prompt /research/reviewer "请评审这个设计" --queue --notify
treer agent ls /research
# issues -> /shared/issues
# reviewer -> /review-team/reviewer
treer agent rm /research/reviewer
# 只删除别名，/review-team/reviewer 仍在运行
```

Core Message sender/recipient IDs、通知目的地、queue ownership、App service audience 和
审计 principal 永远是真实身份。别名不是可 impersonate 的 sender，不能注册 AIS、
startup、凭据或 mailbox。Message/通知路径是接受操作时的规范路径快照；rename/move
不改变历史事件。审计 primary resource 是真实 target，并另附有界
`via_link_id`/别名路径/tree revision 帮助调查，不将 alias 写成 actor identity。
同一 Message 请求中多个 recipient 别名指向同一真实 mailbox 时按规范 ID 去重，
不创建额外投递或绕过 idempotency。接受时已 revoked/disabled 的目标按现有规则拒绝；
不能借链接取回已吊销 credential，也不能因缓存显示 live 就恢复运行。

Controller 不能只在本地把 alias 转成 ID 后丢掉 provenance。拟议请求可带 lookup path、
expected link ID/revision 作为未可信 resolution hint；Proxy 重查并核对，再绑定真实 ID。
对链接解析与动作接受使用一致快照/锁，在等待外部 Policy Provider 后重新核对 link
revision、target active 状态和适用授权 revision；发生变化返回 `tree_changed`，客户端
不能自动改为另一目标。动作接受后 dispatch 固定 ID，沿用 generation fencing；已接收
queue/message 不随链接删除而取消。跨网络不能持有数据库锁等待整个 Agent 回合。

## 可见性与创建、删除权限

建议新增三项 node-level action：`agent.tree.link.create/update/delete`。
创建作用于目的 folder（root 使用 workspace root resource）；更新作用于 link，并对
新目的 folder 求值；删除作用于 link。沿用 `agent.tree.read` 作树读取的入口检查。
folder/link typed resource selector 是新设计，不能假定现有 workspace resource 已提供。
创建者不因 created_by 自动拥有永久删除或跨 folder 写权限；Owner 通过明确管理 grant
操作，权限变化后仍需重新求值。不存在通过删除 target Policy grant 来限制 rm 别名的必要。

创建还必须对目标通过 `agent.discover` 和 `agent.metadata.read`（App 使用规范 workload
metadata 映射，若独立 App metadata 无对应 action，应补动作而非绕过）。只要求发现/最小
元数据，不要求读 transcript、文件或调用 App；知道 UUID 不代表可链接。无需 target Owner
逐个批准别名，因为它不是 grant；高敏对象可通过 discover/metadata denial 禁止创建。
不能把“创建者曾获准”变成所有看此 folder 的人都可见真实目标。

读取别名时每个 caller 重新检查目标 discover/metadata 权限。无权者可看到自己获准的
别名 label，但只显示 `unavailable`，不泄漏 target ID/path/kind、删除状态或名称提示。
`broken` 只向获准管理此 link 且可查看相应 tombstone 的主体显示；一般主体不能借
broken 与 forbidden 的差异枚举隐蔽目标。目标删除后清理/显示应支持管理者，不要求
已经不存在目标的 live metadata permission。避免永久保存完整 target 名称快照给所有人。
别名执行解析同样需当前最小 discover/metadata 权限，再独立检查具体操作；有操作 grant
但无发现权限的主体可按既有稳定 ID 契约调用，不能借 alias lookup 绕过隐藏元数据。

现有 Tree/Agent discovery 数据有 workspace-wide 可见行为，若要声称链接不会枚举隐藏
目标，实施前必须盘点过滤所有 ls、Tree API、错误 hint 和 web 搜索返回，不能只过滤新
`links` 数组。当前 monitor/implicit Allow 仍可能全放行；本设计不改默认，也不声称添加
action 就已有最小权限。参见 [Policy 迁移方案](2026-10-12-policy-enforce-migration.md)。

## 同文件夹规则与显式跨团队授权（只作方案）

示例：`/team-a/mail -> /team-b/mail`。team-a Agent 仍不与 team-b endpoint 在规范
同一文件夹，现有 email-endpoint 应继续 `folder_denied`；反向把 team-a Agent 链接进
team-b 也不能改变 caller 的 folder。树导出必须保证规范 `agents` 中每身份只有一条，
aliases 放 `links`，否则 endpoint 的按 ID 第一项匹配会误用别名 placement。

要让跨团队通信实际可用，需要分别满足：

1. Core 的 message.send/agent.prompt/network.connect 等真实目标权限；
2. App 自己的 channel/data ACL、服务 audience、用户的邮件收件同意等独立条件。

推荐 email-endpoint 保留现有“规范同文件夹”兼容模式，另增加管理员明确配置的
`allowed_agent_ids` 或静态团队主体组。grant 按稳定 Agent ID、可用 endpoint/action、
期限/撤销记录绑定，不按别名位置；文件夹动态组若另做，必须说明移动会改变成员。
显式跨团队模式由 endpoint Owner 开启，不能 `ln` 自动开启或绕过 activation。
核心 Policy 允许网络访问不应等同 endpoint 允许发信；拒绝原因需区分两层。

v1 不必为了 ln 同时修改 email-endpoint。UI/CLI 提示链接只提供入口和规范目标，并展示
权限不足时如何请求授权。后续由 apps-dev 设计 endpoint ACL，core-dev 评审身份边界；
web-dev 提供 grant 入口，Policy 默认不动。跨团队 Core Message/prompt 同样使用明确
现有规则，而不是把同目录别名当成新团队成员。

## CLI 与网页 Tree

JSON `ls` 增加独立 `links` 数组，保留 folders/agents 原形；字段包括 link_id/name/path/
folder_id/position/revision/state，允许时附 target_kind/target_id/target_path。
文案格式是 `名字 -> 目标规范路径`，broken 显示 `名字 -> [broken]`；有管理权限且可见
tombstone 时可附 target ID，不把历史路径当仍可寻址的目标。无权限显示 `[unavailable]`。
CLI 现有 JSON 默认不变；`display` 字段或未来明确的 text renderer 提供箭头展示，
脚本应解析结构字段。排序尊重混合 node 的 position，不能 Agent 排一组、link 另排一组。

Web 节点有 link 图标、别名 label、真实目标路径；选中打开现有真实 Agent/App UI，
标明“通过链接访问”，不创建第二套会话订阅/Agent 状态。通过 link ID 而非 target ID
作为 React node/拖拽 key，避免两个别名互相覆盖。Copy path 提供此别名路径，另有
Copy target path；Open target 跳到规范节点。编辑/删除菜单明确“重命名/移动/删除链接”，
生命周期动作明确目标及确认；broken 不打开最后缓存的 UI。移动不改身份或同文件夹权限。
前端搜索区分 alias 与 canonical，隐藏目标不显示缓存名称；订阅/缓存按 workspace 与
caller 隔离。Desktop/mobile 共用同一授权与确认流程。

## 版本协商与混合版本

拟议能力 `agent.tree-links.v1`，不是沿用 `agent.path-targets.v1` 假装旧解析器支持别名。
Proxy 提供 Core API 能力；Controller 仅在自身解析/传输支持且已连接 Proxy 支持时
向 CLI 广告有效能力。创建、解析、读 link 等在 dispatch 前检查两端；缺失返回
`tree_links_unsupported`，不回退插入、全局名字匹配或把 link 字符串当 Agent ID。

旧 CLI 对新 Controller：传统 ID/规范路径继续可用；Controller 能识别输入是别名时
返回明确升级 CLI 错误，而不执行潜在 rm/admin 操作。CLI handshake 新增可选 features
字段，缺字段即 legacy，未知能力可忽略，不更改现有帧的必填枚举导致连接断开。
旧 Controller 对新 CLI：新的链接命令及别名操作明确拒绝，普通规范目标不应全部禁用。
旧 Controller 不能懂别名时让 Proxy 禁止将其 alias ID/path 转译为任意同名真实目标。

旧 Tree JSON consumer 可忽略新增 links，只看到规范对象；新版消费者不能将遗漏的
links 误当成可删除空目录。是否为空、名称保留及 destructive 操作由新 Proxy 权威校验。
旧 web 没有链接 UI不影响目标运行。旧 Proxy 二进制没有 links 冲突检查，不能在启用
功能的 workspace 中继续写 Tree：部署先全 Proxy 完成兼容代码、再 Controller/CLI/web、
最后启用 workspace feature flag；回滚到完全不理解 links 的 Proxy 必须先关闭树写入口
并安排 operator 清理/迁移，不能静默保留可碰撞别名。链接只是元数据，不要求重启 Agent。

## 安全风险与验收

| 风险 | 约束与必须验证 |
| --- | --- |
| Confused deputy / 伪装团队成员 | 真实 ID/action 授权、规范 placement、sender 服务令牌不可自报；链接 App 不获 Agent 能力 |
| 通过链接/错误枚举目标 | 逐 caller 过滤 metadata、hint、broken/tombstone、web 缓存；跨 workspace 创建与读取拒绝 |
| 别名被替换、解析/授权竞态 | 不原地 retarget，link revision CAS，一致树快照，接受时固定真实 ID；并发删除/创建同名不切目标 |
| rm/mv 误删目标 | node 操作不跟随；目标管理显式 follow；不接受 folder link 或 suffix path |
| 名称遮蔽与视图注入 | 三类统一 reservation、历史冲突拒绝、label text rendering/终端转义、无 -f 静默覆盖 |
| NATS 缓存/多 Proxy 偏差 | Postgres 权威、revision/outbox/重取；未知功能 fail closed，无 stale cache 授权 |
| 资源耗尽 | link 数量/每 folder 项数/创建速率限额、分页与批量 target lookup，避免每项独立 Policy 网络请求 |
| 审计无法归因 | 真实 principal/target IDs + via_link_id/接受时规范路径，记录 create/move/delete/failed action；不记 token/prompt 内容 |

回归矩阵包含：两种 target kind、App runtime 重建/离线、跨 workspace、权限变化、不可见
target、不存在/已删目标、同路径新 ID 不复活、alias of alias 归一化、文件夹/link cycle
拒绝、bare/relative/self/ID、ls -R 不重复身份、folder 提升/混合排序、并发 ln/mv/rename/
delete 与动作接受、queue 完成通知身份、email 规范同 folder 拒绝、旧版本握手/错误与
旧 consumer rmdir 拒绝。此处列的是待实现测试，不声称已经运行。

## 分阶段与改动大小

| 阶段 | 交付与 owner | 大小 |
| --- | --- | --- |
| 0：冻结契约 | Core 完成 typed target/App-ID 映射、命令 follow 矩阵、可见性/授权动作和历史冲突扫描设计，web/apps/release-eng 交叉评审 | S–M |
| 1：Core 持久化与解析 | shared protocol、Postgres links/revision、统一 reservation、Proxy 授权/resolve、CAS/事件及 isolated DB tests | M–L |
| 2：CLI 和协商 | Controller lookup/ID dispatch provenance、ln/readlink/rm/ls/mv、follow 确认、旧 Proxy/Controller/CLI 测试；无 Host 进程层变化 | M |
| 3：网页发现体验 | web-dev 图标/别名打开/混合拖拽/创建与删除、Desktop/mobile Playwright 和隐藏 metadata | M |
| 4：可选跨团队授权 | apps-dev 的 email-endpoint 静态 grants/撤销/激活与 Core Policy 配套，不由 links 隐式启用 | M，独立功能 |
| 5：发布启用 | release-eng 按兼容代码先行→全 Proxy→客户端→workspace flag，验证 canonical-only 兼容与回滚；leader 决定启用 | S–M |

总量 M–L，而不是单个 `ln` 命令的小补丁。v1 可先完成只读展示与低风险 prompt/message
寻址，生命周期通过别名继续明确拒绝；待显式 follow 契约和测试完成再开放管理写入。
公开能力只能广告实际支持的阶段，不能先声明全 v1 后让危险子命令各自猜语义。

建议用户确认三点：管理写入是否接受 `--follow-link` 的明确保护；默认发现但拒绝使用
是否符合预期；是否把跨团队 email grants 作为第二个独立功能。本设计推荐全部保留，
不扩大当前 Policy 默认、不把“分身”宣传为新的身份或隔离。
