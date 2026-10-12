# OA 对接规范

> 本文档面向**信息科 / OA 系统集成方**：把院内 OA（办公自动化系统）的决议流转事件接入
> `resolution-watch` 院务会决议督办台账。本工具**只读** OA，按既定 ISO 周次拉取过去一周
> 的流转事件，不会回写 OA。Secretary / 科长签字后由 OA 端按既定动作推进决议。

---

## 1. 6 字段规范（本工具零事件模型）

任何院内 OA 接入都需提供这 6 个字段，缺一即丢字段，触发 `OaPullError::SchemaMissing`。

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `oa_event_id` | string | ✓ | OA 端唯一事件 id（去重用）。同一事件多次回放应保持 id 不变。 |
| `resolution_id` | string | ✓ | 关联的决议编号（与 `inbox/<week_id>/minutes.md` 决议条目编号严格对齐）。 |
| `action` | enum（string） | ✓ | 流转动作，只接受 5 个值：`started` / `in_progress` / `blocked` / `submitted` / `completed`。 |
| `actor_id` | string | ✓ | 触发人 / 工号。OA 返回字段若是姓名，需对接方提供姓名→工号映射或回填工号。 |
| `event_time` | RFC3339 string | ✓ | 事件时间戳，`YYYY-MM-DDTHH:MM:SS+08:00` 形式（含时区）。 |
| `status` | string | ✗（本工具忽略） | OA 端自有状态字段（与 `action` 重叠但不完全等同），本工具**不读** `status`，仅读 `action`。若 OA 端 schema 仅含 `status` 而无 `action`，对接方须提供 `status → action` 映射（本工具的 5 态枚举）。 |

> **为什么本工具以 `action` 为准**：`action` 是动词语义（"启动/推进/阻塞/提交/完成"），可以直接推导状态机跃迁；
> `status` 是状态语义（OA 端内部流转标识），各家 OA 命名差异极大。本工具的 5 态状态机
> （pending → in_progress → due_soon → overdue → completed）由 `action` 驱动，与 OA 端 `status` 解耦。

---

## 2. 4 类院内 OA 字段映射

下表把 4 类常见院内 OA 的**原始字段**映射到本工具的 `OaEventDto` 6 字段。
若贵院 OA 系统不在下表中，参考「5. 自研 OA 适配模板」一节自行映射。

### 2.1 蓝凌（Landray）EKP

蓝凌 EKP 是医院 OA 高频选型，工作流引擎表 `wf_history` / `wf_instance` / `process_history`。

| 本工具字段 | 蓝凌字段 | 表 / 视图 | 说明 |
|-----------|----------|-----------|------|
| `oa_event_id` | `wf_history.id` | `wf_history` | 主键 BIGINT，转字符串 |
| `resolution_id` | `wf_instance.biz_id` 或 `wf_history.biz_field1` | `wf_instance` | 关联业务单据编号；与会议纪要决议编号拼接 |
| `action` | `wf_history.action_name` | `wf_history` | 见下方「action 中文 → snake_case 映射」 |
| `actor_id` | `wf_history.userid` | `wf_history` | 蓝凌人员 id 即工号 |
| `event_time` | `wf_history.createtime` | `wf_history` | BIGINT 毫秒时间戳 → 转 RFC3339 |

### 2.2 致远（Seeyon）A8

致远 A8 工作流引擎表 `form_node` / `wf_process` / `collaboration_history`。

| 本工具字段 | 致远字段 | 表 / 视图 | 说明 |
|-----------|----------|-----------|------|
| `oa_event_id` | `collaboration_history.id` | `collaboration_history` | 主键 |
| `resolution_id` | `collaboration.biz_id` 或 `form_main.biz_id` | `collaboration` / `form_main` | 业务编号 |
| `action` | `collaboration_history.action_name` | `collaboration_history` | 见下方映射 |
| `actor_id` | `collaboration_history.user_id` | `collaboration_history` | 致远人员 id |
| `event_time` | `collaboration_history.create_date` | `collaboration_history` | TIMESTAMP → RFC3339 |

### 2.3 泛微（Weaver）e-cology

泛微 e-cology 工作流引擎表 `workflow_currentoperator` / `workflow_nodelog` / `workflow_requestbase`。

| 本工具字段 | 泛微字段 | 表 / 视图 | 说明 |
|-----------|----------|-----------|------|
| `oa_event_id` | `workflow_nodelog.id` | `workflow_nodelog` | 主键 |
| `resolution_id` | `workflow_requestbase.requestid` 或 `requestname` | `workflow_requestbase` | 流程实例编号 |
| `action` | `workflow_nodelog.operatetype` 或 `workflow_nodelog.remark` | `workflow_nodelog` | 见下方映射 |
| `actor_id` | `workflow_nodelog.operator` | `workflow_nodelog` | 泛微人员 id |
| `event_time` | `workflow_nodelog.operatedate` + `operatetime` | `workflow_nodelog` | DATE + TIME → RFC3339 |

### 2.4 自研 OA / 其他厂商

参考「5. 自研 OA 适配模板」一节。任何能提供 6 字段需求的 OA 均可接入。

---

## 3. action 字符串映射（OA 中文 → 本工具 snake_case）

本工具 5 态 action 枚举只接受 snake_case 字符串。对接方负责把 OA 端的中文动作名映射到下方 5 个值之一：

| 本工具 action | OA 中文动作示例（各家命名差异） | 状态机效果 |
|--------------|-------------------------------|-----------|
| `started` | 启动 / 发起 / 已派发 / 提交申请 / 创建任务 | `pending → in_progress` |
| `in_progress` | 处理中 / 推进 / 部门办理 / 主办 / 当前节点 | 保持 `in_progress` |
| `blocked` | 阻塞 / 挂起 / 待补充材料 / 待回复 / 待重新提交 | 保持 `in_progress`，但记入审计 |
| `submitted` | 提交 / 送审 / 转下一步 / 转交 / 上报 | `in_progress → due_soon`（若 due_in_days ≤ 3） |
| `completed` | 完成 / 已办结 / 归档 / 通过验收 / 关闭 | 任意 → `completed` |

> **无法映射时**：`OaPullError::SchemaMissing` 立即返回，调用方排查映射表。
> 对接方可在 OA 与本工具之间加一层**适配网关**，把任意中文动作归类到上方 5 值。

---

## 4. REST 端点规范（本工具 →  OA）

本工具默认按下方 REST 协议拉取事件。对接方需在院内 OA 网关层实现该 REST 端点，
内部可走任意数据库 / 工作流引擎，外部签名稳定即可。

### 4.1 端点定义

```http
GET {OA_QUERY_DSN}/oa/events?week_id=2026-W41&page_size=100&cursor=eyJ0IjoxNzA2fQ==
Authorization: Bearer {OA_API_BEARER}
User-Agent: resolution-watch/{CARGO_PKG_VERSION}
Accept: application/json
```

### 4.2 响应结构

```json
{
  "events": [
    {
      "oa_event_id": "WH-2026-10-12-000123",
      "resolution_id": "#[W41]01",
      "action": "started",
      "actor_id": "E10086",
      "event_time": "2026-10-12T09:30:00+08:00"
    }
  ],
  "next_cursor": "eyJ0IjoxNzA2fQ=="
}
```

### 4.3 翻页规则

| 项 | 默认 | 上限 |
|----|------|------|
| `page_size` | 100 | 500（超出 500 强制截断到 500） |
| 翻页上限 | — | 50 页（防无限循环；超出报 `OaPullError::Pagination`） |
| 翻页方式 | `next_cursor` 透传 | — |
| 末页标识 | `next_cursor: null` 或 `next_cursor: ""` | — |

> **`next_cursor` 实现**：可以是 base64 编码的任意字符串（OA 端解析内部状态）。
> 本工具**不解析** cursor 内容，只透传。

### 4.4 鉴权规范

| 项 | 规范 |
|----|------|
| 协议 | `Authorization: Bearer <token>` |
| token 来源 | 环境变量 `OA_API_BEARER`（不含前缀 `Bearer `） |
| 缺失处理 | 启动时 `OaPullError::NotConfigured("OA_API_BEARER")` 立即报错（CLI 退出码 1） |
| 401 / 403 | 立即返回 `OaPullError::Auth`，**不重试**（token 失效需人工处理） |
| 5xx | 指数退避重试（base 500ms / cap 8s / max 5 次），耗尽返回 `OaPullError::Server5xx` |
| 4xx 非鉴权 | 立即返回 `OaPullError::Client4xx` |
| 连接错误 | 同 5xx 重试策略 |

### 4.5 凭据轮换

- `OA_API_BEARER` 在院内部署时建议走 KMS / Vault 凭据中心，按 8–24 小时轮换
- 本工具每次启动读 1 次 env，**不刷新** token；token 失效后下次启动生效
- 401 报错会立即让本周次决议督办失败（CLI 退出码 2），运维收到告警后手动更新 KMS

---

## 5. SQL 视图规范（无 REST 网关时的备选）

部分院内 OA 不便提供 REST 网关，可由 OA 厂商直接在院内数据库层暴露**只读视图**。
本工具在 `OA_QUERY_DSN` 形如 `sqlserver://user:pass@host:port/db?view=oa_events_v` 时
走 SQL 拉取（task 后续扩展点；本期未实现）。

### 5.1 视图 DDL 模板（SQL Server）

```sql
CREATE VIEW oa_events_v AS
SELECT
    CONVERT(varchar(64), wh.id)              AS oa_event_id,
    CONVERT(varchar(64), wi.biz_id)          AS resolution_id,
    LOWER(REPLACE(REPLACE(wh.action_name, N' ', N'_'), N'已', N'')) AS action,
    CONVERT(varchar(64), wh.userid)          AS actor_id,
    FORMAT(wh.createtime, 'yyyy-MM-ddTHH:mm:sszzz', 'zh-CN') AS event_time
FROM wf_history wh
JOIN wf_instance wi ON wi.id = wh.instance_id
WHERE wh.createtime >= DATEADD(WEEK, DATEDIFF(WEEK, 0, GETDATE()) - 1, 0)
  AND wh.createtime <  DATEADD(WEEK, DATEDIFF(WEEK, 0, GETDATE()),     0);
```

### 5.2 视图 DDL 模板（MySQL / 蓝凌）

```sql
CREATE OR REPLACE VIEW oa_events_v AS
SELECT
    CAST(wh.id AS CHAR)              AS oa_event_id,
    CAST(wi.biz_id AS CHAR)          AS resolution_id,
    LOWER(REPLACE(wh.action_name, ' ', '_'))  AS action,
    CAST(wh.userid AS CHAR)          AS actor_id,
    DATE_FORMAT(wh.createtime, '%Y-%m-%dT%H:%i:%s+08:00') AS event_time
FROM wf_history wh
JOIN wf_instance wi ON wi.id = wh.instance_id
WHERE wh.createtime >= DATE_SUB(DATE(NOW()), INTERVAL WEEKDAY(NOW()) + 7 DAY)
  AND wh.createtime <  DATE_SUB(DATE(NOW()), INTERVAL WEEKDAY(NOW()) DAY);
```

### 5.3 视图权限

```sql
-- 只读账号（无 view 权限，仅 SELECT）
GRANT SELECT ON oa_events_v TO 'reso_readonly'@'10.x.x.%';
-- 禁止写入
REVOKE INSERT, UPDATE, DELETE ON oa_events_v FROM 'reso_readonly'@'10.x.x.%';
```

### 5.4 字段映射核对表

| 视图字段 | 本工具字段 | 校验 |
|---------|-----------|------|
| `oa_event_id` | 同名 | 同一决议同一流转事件多次查询应保持 id 一致 |
| `resolution_id` | 同名 | 必须与 `inbox/<week_id>/minutes.md` 中决议编号严格对齐 |
| `action` | `OaAction` 5 值之一 | 映射失败 → `SchemaMissing` |
| `actor_id` | 同名 | 工号形式（建议 4–8 位数字） |
| `event_time` | RFC3339 | 时区必须是 `+08:00`（国内）或 `+00:00`（带时区转换） |

---

## 6. 自研 OA 适配模板

院内若有自研系统或不在 4 类之外的厂商，按下方模板实现一个适配层：

```python
# 伪代码：把院内 OA 数据库 → 本工具 OaEventDto
def transform(raw_event):
    return {
        "oa_event_id": str(raw_event["id"]),
        "resolution_id": raw_event["biz_id"],
        "action": map_action(raw_event["status"]),  # 见第 3 节
        "actor_id": raw_event["user_id"],
        "event_time": iso8601(raw_event["created_at"]),
    }

def map_action(oa_status):
    return {
        "已发起":  "started",
        "办理中":  "in_progress",
        "挂起":    "blocked",
        "已提交":  "submitted",
        "已完成":  "completed",
    }.get(oa_status, "blocked")  # 未识别动作落 blocked 不推进状态机
```

---

## 7. 端到端联调示例（信息科先验）

### 7.1 准备阶段

1. **建只读账号** `reso_readonly`（参考 5.3）
2. **创视图** `oa_events_v`（参考 5.1 / 5.2）
3. **拿 bearer token**（OA 网关侧签发）
4. **配置环境变量**：
   ```bash
   export OA_QUERY_DSN="https://oa.hospital.local/api/v1"
   export OA_API_BEARER="<from KMS>"
   export RESO_DB_PATH="./.reso-cache.sqlite"
   export RESO_MINUTES_DIR="./inbox"
   export OUT_DIR="./out"
   ```

### 7.2 干跑验证（不写文件）

```bash
# 1. 把上周纪要 md 落 inbox
cp minutes-latest.md

# 2. 起 mock OA + mock LLM（info科环境）
cargo test --manifest-path resolution-watch/Cargo.toml --test oa_pull_mock -- --nocapture

# 3. 联调（mock 替身）：发真实 REST 请求
curl -sS -H "Authorization: Bearer ${OA_API_BEARER}" \
     "${OA_QUERY_DSN}/oa/events?week_id=2026-W41&page_size=10" | jq .
```

### 7.3 真跑验证

```bash
cargo build --manifest-path resolution-watch/Cargo.toml
cargo run --manifest-path resolution-watch/Cargo.toml -- \
    --week 2026-W41 run

# 预期：out/2026-W41/ 下生成 6 个文件
ls -la out/2026-W41/
# resolutions.csv  overdue.jsonl  due_soon.jsonl
# conflicts.jsonl  summary.json   audit_log.jsonl  sha256_manifest.json
```

### 7.4 验收对照

| 验收项 | 命令 | 期望 |
|--------|------|------|
| bearer 注入 | `OA_API_BEARER=xxx cargo run …` | 报文 Authorization 头含 `Bearer xxx` |
| 4 类 OA 映射 | 查 `out/<week_id>/audit_log.jsonl` | `oa_pull_complete` 段含 `events_count` 与本周次流转事件数一致 |
| 翻页上限 | 设 51 页 mock OA | `OaPullError::Pagination` 立即退出码 2 |
| 鉴权失败 | `OA_API_BEARER=invalid` | 立即退出码 2，不重试 |
| 5xx 重试 | mock OA 第 2 次返 503 | 第 3 次返 200，本工具重试不报错 |
| 端到端 | `cargo run -- run --week 2026-W41` | `out/<week_id>/` 6 文件齐全，sha256 manifest 与文件一致 |

---

## 8. 不在本工具边界内的能力

- **OA 端 schema 演进**：OA 端字段新增 / 改名词，本工具不动；由对接方负责更新映射表
- **OA 端权限接入**：bearer token 签发 / 轮换由 KMS / 网关侧负责
- **决议状态回写 OA**：秘书 / 科长签字后由 OA 端按既定动作推进（本工具**不写回 OA**）
- **历史流转回填**：本期只支持查询上一周 (`week_id` = `now - 1 ISO week`)，跨周回填不在本期任务内

---

## 9. 故障排查速查表

| 现象 | 可能原因 | 排查命令 |
|------|----------|----------|
| `NotConfigured("OA_API_BEARER")` | 环境变量未设置 | `echo $OA_API_BEARER` |
| `NotConfigured("OA_QUERY_DSN")` | 同上 | `echo $OA_QUERY_DSN` |
| `Auth` 错误 | bearer 失效 | `curl -H "Authorization: Bearer $OA_API_BEARER" $OA_QUERY_DSN/ping` |
| `Client4xx` 错误 | REST 端点 schema 错（如 cursor 拼错） | `curl -v …` 看响应体 |
| `Server5xx` 重试耗尽 | OA 后端挂 | 联系 OA 厂商 |
| `Pagination` 翻页超过 50 页 | 单周流转事件 > 5000 | 检查 OA 端 view 是否带周次过滤 |
| `SchemaMissing` action 解析失败 | OA 中文动作未映射到 5 值 | 更新「action 字符串映射」表 |

---

## 附：术语表

| 术语 | 含义 |
|------|------|
| `action` | 流转动作（动词语义），snake_case 5 值之一 |
| `status` | OA 端自有状态字段（名词语义），各家命名差异大；本工具不读 |
| `oa_event_id` | OA 端唯一事件 id（去重用） |
| `resolution_id` | 决议编号，与会议纪要决议条目编号严格对齐 |
| `next_cursor` | 翻页 cursor，本工具只透传不解释 |
| `OA_QUERY_DSN` | OA 端点 base URL（含协议 + port + 路径前缀，**不含**具体资源路径） |
| `OA_API_BEARER` | 鉴权 token（**不含** `Bearer ` 前缀） |