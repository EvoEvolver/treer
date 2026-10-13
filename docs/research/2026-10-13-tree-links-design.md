# Agent Tree 软链接（分身）设计

状态：设计审查，未实现；按用户2026-10-13最新决定恢复为“只负责发现与寻址，授权完全由Policy”。
此版替代此前的链接共享授权提案，历史保留在Git，不作为实施依据。只改文档，不改默认权限。
配套见[Policy路径规则与App动作设计](2026-10-13-policy-path-rules-design.md)。

## 边界

链接是文件夹内有名字的别名节点，指向同workspace已有Agent或Managed App，不复制进程、
身份或credential。v1不链接folder、不跨workspace、不存link-to-link：target若是别名先
解析为真实稳定ID。只有一份真实规范placement，alias不是caller的新folder或App audience。
链接存在/创建/移动/删除均不授予或撤销目标权限。相同caller按alias或真实ID访问应获得
同一Policy结果。需要数据隔离时建立独立App，链接不是沙箱。

建议node rm/mv永不跟随；经过alias做真实target生命周期管理默认要求显式--follow-link，
或v1先明确拒绝。这是防误操作确认，不是Policy授权，也不替用户规定谁必须是owner。

## 现状核对

- [树模型](../../crates/treer-protocol/src/workload_tree.rs)只有folders/placements，每真实身份一份。
- [resolver](../../crates/treer-protocol/src/targets.rs)使用规范caller folder解析relative，operator从root；裸名仅兄弟，无全局fallback。
- [树持久化](../../crates/treer-proxy/src/auth/workload_tree.rs)锁workspace行，但folder/workload名称检查分离，需统一三类reservation。
- [树授权](../../crates/treer-proxy/src/api/workload_tree.rs)当前agent.tree.read/manage为workspace级，尚无路径选择器。
- [CLI ls](../../crates/treer-agent-server/src/cli_workload_tree.rs)输出JSON folders/agents，links不能伪装成第二份Agent。
- [email-endpoint](../../apps/email-endpoint/email_endpoint.py)当前自行查真实ID/folder并硬编码同目录，需配套App动作检查替代。

## Core/Postgres/NATS模型

新增workload_tree_links，字段workspace_id、link_id(link_UUID永不复用)、folder_id、name、
position、target_kind、target_id、revision(CAS)、created_by_kind/id、created_at/updated_at。
无grant字段/授权版本/credential。Agent target保存Agent ID，App保存稳定deployment App ID，
按动作映射当前runtime/service，不能保存PID、generation或只按名字猜audience。独立network
service暂不当App link，未来需明确target kind。

target_path/name/status查询时派生。停止/离线不等于broken；真实删除保留target ID和link，
不使用target级联删除FK。同路径新对象不同ID不能复活旧link。workspace/folder用复合约束。
v1不原地retarget，换目标删旧link/建新ID；rename/move增revision但保留link ID。

创建link、删除target、rename/move共用workspace事务锁，检查target存在、归属与kind。
统一reserve_tree_label对folder/workload/link同目录大小写敏感唯一；root NULL单独处理。
历史同名只读报告、不自动改名/覆盖，路径报冲突、ID仍可用。folder提升先检查最终集合。
新增名字保留ID前缀/self/dot，禁止slash/control字符，防终端/HTML注入。

workspace tree_revision与变更同事务递增，无内容变化不递增。Postgres权威，NATS只传播
workspace/revision/node ID的失效事件，推荐transactional outbox；断线/缺口重取权威快照。
不维护第二份link或授权状态，不以NATS/cache独立授权；配套Policy使用当前revision。

## 解析与命令矩阵

保持absolute/relative/bare/self语义；alias仅可为最终组件，alias/child报not_a_directory。
folder resolver不跟随；ls -R列link一次，不递归target。caller cwd永远规范folder，
不会因为prompt经alias送来而改变。resolve_node不跟随，resolve_workload允许单跳，
返回node/link ID、path/revision、真实target kind/ID/path和tree_revision。

| 操作 | 行为 |
| --- | --- |
| agent prompt/queue/output/transcript/show | 跟随真实目标，按真实action/ID/规范path求Policy，App无AIS拒绝，queue ownership照旧 |
| message send --to | 跟随真实mailbox；App仅支持已有recipient类型，否则拒绝 |
| treer exec --machine / SSH machine | 不接受Agent/App link，机器命名空间独立，不自动推导machine.exec权限 |
| app show/UI/地址发现 | 映射稳定App/service，不新建virtual host/公网地址 |
| token create / network | 真实service audience与发行/network权限检查，不把alias当Host或JWT audience |
| target start/stop/restart/admin startup/rename/delete/app update | 默认明确link_target_confirmation_required；显式follow后正常Policy/生命周期校验，不增加grant |
| agent ln target link-path | 保存最终真实ID，destination存在失败，v1无-f覆盖 |
| agent readlink | 查看link与获准target metadata，不读宿主文件 |
| agent rm link | 只删alias，broken可删；非link拒绝，真实删除沿用admin delete/app delete |
| agent mv link destination | 只移动/改名alias，不改target规范位置，不覆盖，不把destination link当folder |
| rmdir / folder删除 | link计为子节点，空目录模式拒绝；提升只移动link节点，整体命名/Policy校验后提交 |
| ls / ls -R | 名字 -> 目标规范路径，broken标记，不重复真实Agent身份 |

示例（未实现）：

~~~sh
treer agent ln /infra/email-endpoint /teamB/mail
treer agent ls /teamB
# mail -> /infra/email-endpoint
treer agent rm /teamB/mail
# 只删入口；授权规则仍由Policy决定
~~~

sender/recipient、queue owner、通知目的地、App audience均真实ID。历史path是接受时规范
快照，改名不回写；同Message多个alias指向同mailbox按ID去重。审计primary resource真实
target，另附via_link_id/alias path/tree revision；node mutation审计link本身。

Controller提交lookup path/expected link ID/revision作为未可信hint，Proxy重查真实target；
动作接受与Policy核对一致currentTree及active binding，变化报tree_changed而非自动换对象。
接受后dispatch固定ID/generation；rm不取消已接收queue/message。App延迟副作用执行前
重查Policy，跨进程撤销不宣称回滚已执行操作。

## 权限由路径Policy决定

不再内置target管理资格要求或链接自动授权。ln需要有效同workspace身份、target存在且
可discover/最小metadata、目的写操作获Policy允许；知道UUID不代表可枚举资源。
与配套v2动作对齐：tree.link.create/delete/update携带link源/目的path与target引用，
tree.move/rename携带before/after transition。谁可ln/mv以及是否特权由用户规则决定，
不推导文件夹成员获得使用grant。created_by也不意味着永久管理权。

Owner本workspace Tree管理break-glass按配套契约永远可救援，不绕过workspace/身份/
合法树约束。当前monitor/defaultAllow仍可能允许普通操作，实际mode如实显示，不改默认。
各caller重新检查Tree与target discover/metadata，无权只见获准alias label/unavailable，
不泄漏target ID/path/kind/删除状态/hint；broken仅管理者且可见tombstone时标记。
删除broken不要求不存在target的live权限。过滤还需覆盖既有ls/search/hints/web缓存。

规则例：允许规范subject /teamB/* 的Agent对规范target /infra/email-endpoint执行app.use
与email.send。*仅直属、**含子树，均是Policy表达而非ln默认。有link但规则拒绝不能用，
无link但规则允许仍可直接访问。alias移动/删除不改变这些使用规则；target规范移动/改名
按当前路径改变匹配，path与stable-ID规则区别见配套文档。

verify_app只返回Core生成的principal/resource tree context与revision，身份active不是
某action允许；不返回链接授权判断或共享folder列表。email-endpoint改向App policy check
询问app.use/email.send，不再硬编码同folder/自行查树，保留human管理、activation、
收件同意与业务限制。按配套stale/fail_open/cache契约处理，60秒JWT不是即时授权撤销。
未接入check或离线App不能宣传受Core Policy完整约束。

## Policy App规则草稿建议

可由用户选用Policy App提供“创建链接后生成规则草稿”：取规范caller folder与target，
让用户选直属*或子树**、action范围，展示priority/已有规则和改名影响；Owner审核后
publish新revision。只是App层建议，不写死Core、不自动grant，也不建立grant表。
ln与publish不是跨服务原子事务，UI分别显示结果；规则失败alias可留下但不能称已授权。
rm可建议撤回关联rule草稿，但不能自动删共享规则/其它人权限；Policy App记录rule ID关联。

## Web/CLI与旧版本

ls JSON保留folders/agents，独立links数组含link_id/name/path/folder_id/position/revision/
state；获准时附target_kind/ID/path。显示名字 -> 目标规范路径；断开名字 -> [broken]，
无权unavailable。保持现有JSON默认，结构字段独立于display，混合position排序。

Web link图标/alias label/规范path，选中复用真实Agent/App UI，标明经alias访问。
React/拖拽key用link ID，防同target多个alias覆盖。Copy path复制alias路径，另可Copy
canonical target；删除/移动明确是link，生命周期写操作确认真实对象；broken不打开旧缓存。
不显示“创建即共享/删除即撤销”。失败引导Policy explain，规则草稿入口可集成Policy App。
desktop/mobile一致，cache按caller/workspace隔离。

能力agent.tree-links.v1由Proxy/Controller共同支持才广告，缺失tree_links_unsupported，
不退回全局名字匹配/猜link ID。旧CLI缺feature时新Controller对alias操作明确升级提示。
handshake可选features未知忽略，不引入旧连接无法解码的必填枚举。
JSON旧consumer可忽略links，但rmdir空判断/namespace冲突由Proxy权威处理。
全Proxy兼容→Controller/CLI/web→workspace flag；旧Proxy不懂reservation不能继续写Tree，
回滚前关闭Tree写并安排处理，不静默制造碰撞。pure alias不要求Agent重启。

路径Policy与App check分别协商。alias可以独立发布为发现能力；旧ID规则继续按原mode，
旧email-endpoint仍硬编码同folder，不能宣传支持新跨team路径授权。完整App体验需配套
阶段4升级email-endpoint；其后规则管理完全由用户Policy App负责。

## 风险、测试与计划

| 风险 | 约束 |
| --- | --- |
| Confused deputy | 真ID/规范path求值，alias非sender/folder身份/audience，App check绑定真实实例 |
| 枚举目标 | 按caller过滤metadata/hints/缓存，broken不泄漏，跨workspace拒绝 |
| 解析竞态/替换 | 不原地retarget，CAS/一致快照，固定ID，alias/canonical同决策 |
| rm/mv误删target | node操作不跟随，目标管理明确确认且Policy求值 |
| namespace/注入 | 三类reservation/历史冲突报告，text渲染及终端转义 |
| 规则草稿误扩权 | App审核priority/action/*与**范围，ln/publish分开，无自动规则发布 |

测试：两target kind、App runtime重建、offline/broken/新ID不复活、folder/link环拒绝、
bare/relative/self、三类冲突/混合排序/隐藏metadata、并发ln/mv/delete/dispatch、Message
去重/via_link、旧版本明确拒绝。必须证明alias创建/移动/删除不改变规范target使用Policy，
App由check而非alias或同folder硬限制决定权限。此处为待实现测试，本次仅文档检查。

| 阶段 | 交付 | 大小 |
| --- | --- | --- |
| 0 | Core/web/apps/release-eng冻结pure alias模型/follow矩阵/namespace与Policy动作 | S–M |
| 1 | Core Postgres links/revision、reservation、resolver/Proxy/CAS与隔离DB测试，无grant逻辑 | M–L |
| 2 | Controller/CLI ln/readlink/rm/mv/ls、协商/ID dispatch/审计与旧版本测试 | M |
| 3 | web树/拖拽/菜单/target UI复用，desktop/mobile安全展示 | M |
| 配套A阶段1/2/3 | currentTree/path规则/explain与monitor先行，用户自建Policy App | 见配套方案 |
| 配套A阶段4（App同步） | apps-dev email-endpoint verify tree+policy check，移除同folder硬限制，才宣称路径授权可用 | M–L |
| 4 | 可选Policy App草稿建议/审核publish，非Core必需 | S–M |
| 5 | release-eng兼容/flag/回滚验收，leader决定启用 | S–M |

pure aliases规模M–L，不建第二套授权。路径规则/App check按配套方案实施，默认不改，
Owner Provider/Tree救援保留。用户以Policy决定谁可mv/ln，链接不成为身份或授权依据。
