# Agent Tree 软链接（分身）设计

状态：设计审查，未实现；2026-10-13 按用户决定修订为“链接即授权”。基线为 `139efa6`；
本次只修改此文档，不改代码、Policy 默认、数据库或运行中的工作负载。链接同时提供附近
入口和文件夹共享授权，不复制进程或身份。需要数据隔离时使用独立 App 实例。

## 建议与需要质疑的假设

采用“文件夹内有名字的别名节点 → 同 workspace 的稳定 Agent/App ID”。v1 不链接
文件夹、不链接链接、不跨 workspace、不复制 credential，也不创建新 workload。
真实目标只有一份规范 placement；多少个别名都不能改变它。链接所在文件夹的直属 Agent
获得对目标的使用授权，不包括子文件夹、App 身份、human 或其它文件夹的 Agent。
root 的 NULL folder 也只包含根目录直属 Agent。此处“授权”是动态文件夹成员共享，而非
创建时成员名单快照；Agent 移入获得、移出失去，撤销此条共享通过删除链接完成。

保留规范身份、删除后 broken 的方向，修订后的授权模型需要四个约束：

1. 只链接 Agent/App 仍不足以自动排除循环：创建接口必须禁止 link ID 成为 target，
   不能保存另一个别名的路径。用户以别名创建链接时先解析到真实目标并保存其 ID。
2. 创建链接是向整个目的文件夹授权的特权操作，必须同时检查真实目标管理权与目的
   文件夹写权限；只有目标可读权限绝对不够。不能让普通 Agent 自己 ln 就给自己授权。
3. 规范路径是可变的展示信息，不是规范身份。授权、投递、审计关联以稳定 ID 为准；
   Message 路径保留发送时快照，不能随着目标移动回写历史。
4. 现有名称校验没有统一检查 folder 与 workload 的冲突。要求所有节点同目录唯一，
   需统一 reservation，并报告历史冲突；不能只给新 links 加一条唯一索引就称已解决。

建议 v1 对目标执行非破坏性操作自动跟随；节点编辑永不跟随；目标生命周期/管理写入
经过别名时要求显式 `--follow-link`。这是比全命令无条件跟随稍保守的设计，防止用户
将“删除别名”误解成“删除远端团队的服务”。具体命令矩阵见下文。
链接授予“使用”而不是“管理”：App 为该实例的 Agent 使用入口；Agent 为发现、metadata、
prompt/message、受约束 queue 操作及交互读取。不会授予 startup.manage/delete、
machine.exec/file.write、发放身份或再次创建共享链接的权限。queue cancel 的发送方约束
仍保留。Agent prompt 本身可能使目标执行代码，不能将这一使用授权称为只读或沙箱。

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
| `grant_version` | 固定共享语义版本；v1 是所在文件夹直属 Agent 使用目标，不含管理权 |

App 链接存稳定 deployment `app_id`，按操作映射到当前 runtime/service。不能存易变
runtime PID、run_generation 或仅凭名称猜 service。非 Managed App 的独立 network
service 暂不支持；未来需新增明确 target kind，不能暗中视为 App。

`target_path`、live/offline/broken 和 target display name 均由查询派生，不作路由依据。
没有到 target 的级联删除 FK，否则无法保留 broken 节点；workspace/folder 引用仍用
同 workspace 复合约束。创建时在事务内核实 target 存在且所属 workspace，目标删除
路径与创建路径共用 Tree lock，保证提交时可线性排序。离线/停止不等于 broken。
真实删除留下 target ID；同路径新建一个不同 ID 的对象不得让旧链接复活。

v1 不支持原地 retarget；换目标必须删旧链接、创建新 link ID。rename/move 可以保留
link ID 并增加 revision；move 是撤销旧文件夹共享并向新文件夹授权，不能只检查普通
节点可写。目标不可用/已吊销的链接不产生 entitlement，broken 链接永远不授予使用权。

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
| `agent prompt`, queue read/cancel, output/transcript/show | 自动跟随真实目标，逐项计算现有 Policy 与真实 folder-link 使用授权；App 不支持 prompt/AIS 就明确拒绝，queue ownership 约束不取消 |
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
| `agent mv <link> <destination>` | 只移动/改名 link；不修改真实 placement/name；移动改变授权范围，按撤销旧 grant + 创建新 grant 双重授权，不能覆盖 |
| `agent rmdir`/folder 删除 | link 计作子节点；空目录删除拒绝。提升 links 到父目录会给另一批 Agent 授权，必须逐目标执行创建特权检查，否则拒绝提升，不自动扩大 grant |
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

## 可见性、特权创建与撤销

建议动作 `tree.link.create` 对真实目标求值，`tree.folder.write` 对目的 folder 求值；
新增 typed folder resource，root 使用 workspace root resource。现有 workspace 级
`agent.tree.manage` 不能冒充已存在的细粒度 folder ACL。

创建必须满足：同 workspace 有效身份 + 真实目标管理权（owner/admin，或 owner 明确
委托的目标管理能力）+ `tree.link.create(target)` + `tree.folder.write(destination)`。
普通可读、可使用或另一个 link 的受益者不是管理者。现有 Policy monitor/implicit Allow
不能单独作为“具有管理权”的证明；新特权入口必须有真实管理资格的硬检查，显式委托
需可验证的目标管理授权，不能将 unmatched Allow 解释成委托。当前 Core workspace
role 主要是 owner/member，admin 资格的持久来源与委托模型需先冻结，不接受客户端自报。
这不改变已有 Policy 默认，而是新 grant 创建接口的安全前置条件。

删除允许 `tree.folder.write(link.folder)` **或**真实目标管理权；不要求两者同时满足。
删除 broken link 可由 folder writer 完成；目标删除后可由保留的 owner/admin 管理上下文
清理。删除授权需支持逐条撤销：删除一个 link 不影响同目录其它指向同目标的 link。
最后一个匹配 link 被删才撤销此途径；规范同文件夹、已有 Policy grant 等其它合法途径
仍可能允许，UI 必须显示“撤销此共享”，不能承诺删除一个入口就禁用所有访问。

同目录 rename 只检查 folder write 或目标管理资格，不改变受益者；跨目录 move 同时
满足旧 link 删除条件与新目录的完整创建条件。folder 提升 links 也视作新 grant；任何
目标管理检查失败整笔回滚。将 Agent/App 自身移入有 link 的 folder 是成员资格变化，
必须检查真实目的 folder 写入/placement 管理权限，不能用 monitor 默认放行作为自助
加入特权文件夹的依据。文件夹管理员能调整直属成员，应在 UI 明示其对共享授权的影响。
整个 folder 移动保留 folder ID，直属成员不变，不继承新父 folder 的授权。

链接给予受益 Agent 最小发现与使用元数据，因此它们可以看真实目标规范路径；目录中的
human/App 或非受益主体不能因看到 alias 自动获得使用权。管理者的创建能力同样不是
自身额外使用 grant；授权仍按实际 caller kind/folder/action 计算。

读取别名时每个 caller 重新检查目标 discover/metadata 权限。无权者可看到自己获准的
别名 label，但只显示 `unavailable`，不泄漏 target ID/path/kind、删除状态或名称提示。
`broken` 只向获准管理此 link 且可查看相应 tombstone 的主体显示；一般主体不能借
broken 与 forbidden 的差异枚举隐蔽目标。目标删除后清理/显示应支持管理者，不要求
已经不存在目标的 live metadata permission。避免永久保存完整 target 名称快照给所有人。
别名执行解析可使用直属 Agent 的 link-derived discover/metadata 权限，再计算具体操作；
授权不依赖请求是否真的写了 alias，知道真实 ID 的同一受益 Agent 也应获得相同结果。

现有 Tree/Agent discovery 数据有 workspace-wide 可见行为，若要声称链接不会枚举隐藏
目标，实施前必须盘点过滤所有 ls、Tree API、错误 hint 和 web 搜索返回，不能只过滤新
`links` 数组。当前 monitor/implicit Allow 仍可能全放行；本设计不改默认，也不声称添加
action 就已有最小权限。参见 [Policy 迁移方案](2026-10-12-policy-enforce-migration.md)。

## App 的可信身份验证与 email-endpoint

示例：`/team-a/mail -> /team-b/mail` 授权 team-a 直属 Agent 使用 team-b 的 mail 实例。
它不伪造 caller folder，也不使 team-a 子目录成员获权。反向链接一个 Agent 到 endpoint
所在目录不会授权该 Agent 使用 endpoint：grant 是“link 所在 folder 的 Agents 使用
link 的 target”，不能倒转含义。规范 `agents` 每身份仍一条，aliases 独立 `links`。

扩展现有 `POST /.treer/apps/identity/verify`；当前
[verify_app_identity](../../crates/treer-proxy/src/api/app_identity.rs) 校验 JWT、service
和 human membership，但没有返回 Tree 授权上下文。建议新增独立可选 `authorization`
对象，保留 `active/claims` 的身份语义，不把“未获 App 使用权”误报成 token 签名无效：

```json
{
  "active": true,
  "claims": {"principal_kind": "agent", "sub": "ag_caller", "workspace_id": "ws_example", "service_id": "svc_mail"},
  "authorization": {
    "version": "tree-links.v1",
    "caller_kind": "agent",
    "caller_is_agent_workload": true,
    "caller_folder_id": "folder_team_a",
    "app_id": "app_mail",
    "app_folder_id": "folder_team_b",
    "same_folder": false,
    "entitled_via_link": true,
    "matched_link_ids": ["link_mail"],
    "tree_revision": 42,
    "checked_at": "2026-10-13T00:00:00Z",
    "valid_until": "2026-10-13T00:01:00Z"
  }
}
```

示例 claims 为省略其它现有字段的节选，非新 token schema。Core 从已验签的 sub/workspace/
service audience 映射真实 caller credential、App deployment/service 与当前 placement，
核对 caller 未删除/吊销且真实 kind 非 App runtime，使用一致 Postgres 快照计算
`caller_folder_id == link.folder_id && link.target_kind == app && link.target_id == app_id`。
实际目标绑定不能信 request/header 中自报的 Agent ID、folder、App ID 或 entitlement。
root 同 folder 比较使用 NULL 安全等值；不存在 placement 与真实 root 不可都以 NULL
猜测为 root。Human 的 caller_folder_id 不适用且 entitlement=false，另走现有 human
认证与 App 角色权限。无法映射到 Managed App 的 service 不返回肯定的链接授权。

推荐返回当前 caller 的 `entitled_via_link` 和 matched link IDs，而不返回指向 App 的所有
folder ID 列表：结果更小，也不泄漏其它团队的共享结构。若以后提供列表，仅目标管理者
可读且分页；App 使用请求不需要它。`authorization` 由 App 后端直接经可信 HTTPS Core
验证端点取得、响应 `no-store`，不能接收浏览器转贴的 JSON 或未验签 header。若暴露可由
持有 service JWT 调用的 verify 路由，也只提供此 token 自身、此 audience 的最小上下文。

email-endpoint 必须同期修改 `principal()/placement()/require_enabled()`：不再自行运行
`treer agent ls` 判断 caller 共享范围；使用 Core 响应的真实 App/caller folder 与
`caller_is_agent_workload && (same_folder || entitled_via_link)`。保留 App activation、
endpoint moved 后再确认、human admin、收件同意和 channel 元数据规则。链接授予同一
App 实例的使用资格，不替用户同意邮件，也不改变人类管理身份；不另建 allowed_agent_ids
或静态 grant 配置。需要团队数据/secret 隔离时建立独立 App，链接本身不提供分区隔离。

### 缓存、60 秒窗口与撤销

v1 推荐授权上下文只在线验证，不把 folder/grant 列表永久写入 JWT。身份 token 仍为
60 秒；每次在线 verify 读取当前树，删除最后匹配链接/Agent 移出后，之后新验证立即
返回 entitlement=false。即使旧 JWT 未过期，也不能离线拿它证明旧 link 仍有效。
Core 的肯定授权不得只信 NATS 缓存；发布/缓存延迟不能追加到承诺的撤销上限。

如果 App 缓存验证结果，`valid_until` 必须不晚于 token.exp、checked_at+60 秒以及其它
revocation 边界，缓存键含 workspace、service、principal 与 token 标识；cache hit 不
滑动延长。这样删除链接后的旧肯定结果最多可继续使用剩余 60 秒窗口，不声称立即撤销。
email-endpoint 发信是敏感外部副作用，推荐每次发信前重新在线验证、肯定结果不缓存，
Core 不可用 fail closed；已被接受或已发送邮件不因 rm 回滚。队列发信应在执行时重查，
不能只依赖排队时 grant。负面结果可短缓存但会延迟新链接生效，需可观察。

第三方 App 若只离线验 JWT 而不调用此新 verify 协议，无法知道 grant 与撤销；不能宣称
支持“链接即授权”。若将来将 grant snapshot 签入 token，必须另定 audience/版本/expiry
和最长 60 秒 stale 行为，不能在 v1 默默增加离线授权。缺授权字段的旧 Core/旧 App 明确
不支持新链接共享；email-endpoint 应报升级/验证不可用，不把缺字段解释成允许。

## Agent 目标与 Policy 组合

Agent link 的受益者也是规范 folder 的直属真实 Agent，按真实 target ID/action 计算
derived grant；message sender/queue owner 不变。当前 Policy 默认 monitor/allow 下，
link 不改变既有 Agent 目标的实际权限，删除 link 也不能撤销原本 allow 的访问。
只有采用 enforce、收紧 implicit Allow 后，它才成为授权依据；本次不修改默认。

建议 enforce 下明确优先级：硬身份/workspace/target active 约束 → enforce 显式 Deny →
现有显式 Allow 或适用 link 使用 grant → defaults。link 可克服默认 Deny，不能覆盖
明确安全禁令、queue ownership 或目标管理权限。创建 link 不是宽泛的 Policy bypass。
App verify 的 entitlement 是共享事实；Core network/identity token 发行还需按这一组合
评估，且 App 保留业务安全约束。要让 enforce 下跨团队可用，token/network/Agent 操作
必须同用 grant evaluator，不可仅 App 检查 link，而 Core transport 仍默认拒绝。

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
生命周期动作明确目标及确认；broken 不打开最后缓存的 UI。创建/跨目录拖拽必须提示
“授权此目录所有直属 Agent 使用目标”；删除显示剩余匹配 link 和其它访问途径，说明
撤销窗口。移动不改真实身份，但改变 link 授权范围，不能按普通别名整理静默处理。
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
最后在支持在线 entitlement 的 Apps 升级完成后启用 workspace feature flag；需发布
verify 能力 `app.identity.tree-entitlement.v1`，App 不能仅靠 CLI 支持来推断已获新契约。
回滚到完全不理解 links 的 Proxy 必须先关闭树写入口
并安排 operator 清理/迁移，不能静默保留可碰撞别名。链接只是元数据，不要求重启 Agent。

## 安全风险与验收

| 风险 | 约束与必须验证 |
| --- | --- |
| Confused deputy / 伪装团队成员 | 真实 ID/action 授权、规范 placement、sender 服务令牌不可自报；链接 App 不获 Agent 能力 |
| 自助授权/移动扩大共享 | create 目标管理硬资格+目标动作+folder write；move/promotion重做create；受益者不获再次授权资格；placement写权限不能靠monitor默认自助加入 |
| 通过链接/错误枚举目标 | 逐 caller 过滤 metadata、hint、broken/tombstone、web 缓存；跨 workspace 创建与读取拒绝 |
| 别名被替换、解析/授权竞态 | 不原地 retarget，link revision CAS，一致树快照，接受时固定真实 ID；并发删除/创建同名不切目标 |
| rm/mv 误删目标 | node 操作不跟随；目标管理显式 follow；不接受 folder link 或 suffix path |
| 名称遮蔽与视图注入 | 三类统一 reservation、历史冲突拒绝、label text rendering/终端转义、无 -f 静默覆盖 |
| NATS 缓存/多 Proxy 偏差 | Postgres 权威、revision/outbox/重取；未知功能 fail closed，无 stale cache 授权 |
| grant 撤销延迟/离线 App | online verify 当前树；缓存期限固定且≤剩余60s，不滑动；email发信执行前重查；旧App不宣称支持 |
| 资源耗尽 | link 数量/每 folder 项数/创建速率限额、分页与批量 target lookup，避免每项独立 Policy 网络请求 |
| 审计无法归因 | 真实 principal/target IDs + via_link_id/接受时规范路径，记录 create/move/delete/failed action；不记 token/prompt 内容 |

回归矩阵包含：两种 target kind、App runtime 重建/离线、跨 workspace、权限变化、不可见
target、不存在/已删目标、同路径新 ID 不复活、alias of alias 归一化、文件夹/link cycle
拒绝、bare/relative/self/ID、ls -R 不重复身份、folder 提升/混合排序、并发 ln/mv/rename/
delete 与动作接受、queue 完成通知身份。授权验收必须包含：只读目标者创建拒绝、普通
Agent 在monitor默认下不能自助创建、管理目标但不能写folder者拒绝、两种删除权限各自
允许、直属vs子目录、rootvs未放置、移入/移出、多个links最后一条撤销、broken不授权、
App runtime重建audience绑定、human/App不能冒充Agent、verify的伪造folder请求拒绝、
email同folder或link允许/跨folder无link拒绝/反向Agent-link不授权、60s缓存时间边界与
在线删除后重查、已排队邮件执行前重查、enforce显式Deny不被link覆盖、旧版本握手/
verify缺字段错误与旧consumer rmdir拒绝。此处列的是待实现测试，不声称已经运行。

## 分阶段与改动大小

| 阶段 | 交付与 owner | 大小 |
| --- | --- | --- |
| 0：冻结契约 | Core 与 apps-dev 冻结target管理资格、folder write、直属Agent grant范围、verify响应与60s窗口；命令follow矩阵/冲突扫描；web/release-eng交叉评审 | M |
| 1：Core 持久化与授权 | shared protocol、Postgres links/revision、统一reservation、特权create/delete/move、Policy derived grants、在线verify entitlement与能力；isolated DB/身份测试。email-endpoint先适配测试fixture，不开放共享 | L |
| 2：CLI 和协商 | Controller lookup/ID dispatch provenance、ln/readlink/rm/ls/mv、follow 确认、旧 Proxy/Controller/CLI 测试；无 Host 进程层变化 | M |
| 3：App同期适配 | apps-dev 修改email-endpoint principal/placement/require_enabled，去掉caller查树，online verify同folder或link授权、发信不缓存/执行前重查、保留activation/收件同意；其它App声明支持范围 | M，启用前必需 |
| 4：网页共享体验 | web-dev 图标/别名打开/混合拖拽、授权范围与撤销提示、受限创建/删除、Desktop/mobile Playwright和隐藏metadata | M |
| 5：发布启用 | release-eng 兼容代码先行→全Proxy→Controller/CLI/web与email-endpoint→双端verify契约验收→workspace flag；包括撤销/旧App拒绝/回滚，leader决定启用 | M |

总量 L，因为这次是新的共享授权边界，不再只是发现入口。v1可分批交付只读展示，但
不能对已创建的link宣称“仅别名”而绕过特权检查，也不能宣称授权生效却遗漏App适配。
生命周期通过别名可继续明确拒绝，待follow契约与测试完成再开放管理写入。
公开能力只能广告实际支持的阶段，不能先声明全 v1 后让危险子命令各自猜语义。

已采纳用户决定：链接即直属文件夹Agent使用授权、特权创建、删链接撤销、Core证明App
entitlement，不再引入另一套显式授权配置。仍需冻结“使用”action清单及管理资格来源，
并如实展示其它既有Allow/同folder途径和最多60s缓存窗口。需要隔离另建App，不把
“分身”宣传为新身份或沙箱；当前Policy默认保持不变。
