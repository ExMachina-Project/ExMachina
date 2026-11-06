# nexus-topology-admin Specification

## Purpose
Nexus 连结体管理页：从纯列表升级为节点增删、连边编辑与拓扑图渲染并存的管理面，让连结体结构可视可编辑。

## Requirements

### Requirement: 节点与连边管理端点

Nexus 管理端点 SHALL 支持节点增删（`POST/DELETE /api/nexus/nodes`）与连边编辑（建立 / 解除节点间连结，`PUT /api/nexus/edges`）；删除含连边的节点 MUST 级联清理其全部连边（返回受影响连边数）。端点遵循 REST 通用约定（404 / 409 / 400）。

#### Scenario: 节点级联删除

- **WHEN** 删除一个有 3 条连边的节点
- **THEN** 节点删除成功、3 条连边级联清理，响应含受影响计数

#### Scenario: 重复连边拒绝

- **WHEN** 对已存在连边的两节点重复建立连结
- **THEN** 返回 409 冲突而非创建重复边

### Requirement: 拓扑图渲染与管理页

WebUI SHALL 提供 Nexus 管理页：拓扑图视图（节点为点、连结为边，含节点名称标注）与列表视图并存可切换；拓扑图 MUST 支持选中节点查看详情与连结清单；编辑操作（增删节点 / 连边）MUST 即时反映到图与列表（同一事实源）。窄屏 MUST 可用（图区可横向滚动而非溢出遮挡）。

#### Scenario: 编辑即时反映

- **WHEN** 用户在管理页新增一个节点并与其余两节点建立连结
- **THEN** 拓扑图与列表即时出现新节点与两条边，无需手动刷新

#### Scenario: 选中查看详情

- **WHEN** 用户点击拓扑图中某节点
- **THEN** 显示该节点详情面板（名称 / 属性）与其全部连结清单
