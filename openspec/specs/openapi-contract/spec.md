# openapi-contract Specification

## Purpose

OpenAPI 契约文档：REST 面的机器可读契约——外部集成、SDK 生成与契约测试的根。

## Requirements

### Requirement: OpenAPI 端点

`GET /api/openapi.json` MUST 返回合法 OpenAPI 3.1 文档（JSON），描述全部已注册 REST 端点（路径、方法、参数、请求/响应结构、鉴权要求）。

#### Scenario: 获取文档
- **WHEN** 以有效鉴权请求 `GET /api/openapi.json`
- **THEN** 返回 200 与合法 OpenAPI 3.1 JSON：`openapi` 版本字段、`info`（标题/版本随产品版本）、`paths` 覆盖全部 REST 端点、`components.securitySchemes` 描述鉴权方式

#### Scenario: 鉴权与其他 API 一致
- **WHEN** `authKey` 已配置且请求未携带有效密钥
- **THEN** 与其他 `/api/*` 同口径返回 401；鉴权豁免面与全局中间件一致

### Requirement: 端点覆盖与准确性

文档的 `paths` 与实际注册路由 MUST 一一对应：无「文档有而路由无」的幽灵端点，无「路由有而文档无」的漏项；每端点 MUST 标注鉴权要求与主要错误响应（400/401/404/409 语义族）。

#### Scenario: 全覆盖校验
- **WHEN** 对运行中实例请求 `openapi.json` 并与实际路由清单比对
- **THEN** 路径 × 方法集合完全一致（静态资源、/ws、MCP 挂载等非 REST 面除外）

### Requirement: CLI 导出

`exm openapi [--out <file>]` SHALL 导出同一份文档到文件（缺省打印到标准输出）；MUST 离线可得（不依赖运行中的网关）。

#### Scenario: 导出文件
- **WHEN** 运行 `exm openapi --out api.json`
- **THEN** 文件内容为合法 OpenAPI JSON，与运行实例 `/api/openapi.json` 的 `paths` 集合一致

### Requirement: 与协议文档交叉引用

`docs/协议与契约.md` 的 REST 章节 SHALL 标注「机器契约见 /api/openapi.json」；OpenAPI 文档的 `info.description` SHALL 反向指明人读协议文档位置。两边新增端点时 MUST 同步更新（列入既有开发规范检查项）。

#### Scenario: 双向可达
- **WHEN** 读任一文档
- **THEN** 能找到指向另一文档的引用
