# image-generation Specification

## Purpose
图像生成产出链（助理面差距矩阵 G1.2）：模型应「画一张/生成图片」经 OpenAI 兼容生图端点产出图片，走既有 outbox 产物链直发 IM 或 WebUI 下载——补齐多模态输出最后一格断链。

## Requirements

### Requirement: image_gen 工具

系统 SHALL 提供模型可调用的 `image_gen` 工具：入参 `prompt`（必填）与 `size`（可选，缺省由生图端点决定）；经模型设置中图像生成能力槽位（`imageGen`）配置的 OpenAI 兼容 `/v1/images/generations` 端点生成图片，落工作区 `.exmachina/outbox/` 并登记为 image 产物，由收束产物链按通道能力直发或降级注记。

- 未配置图像生成能力槽位时，该工具 MUST NOT 出现在下发给模型的工具表中；
- 生成成功后 MUST 向模型回显文件名、尺寸（像素）与字节数，模型不得在回复文本中伪造图片链接；
- 图片解码校验失败（返回字节不是合法 PNG/JPEG/WebP）MUST 报错不登记产物。

#### Scenario: 生成并投递

- **WHEN** 模型调用 `image_gen`（prompt 合法）且 imageGen 槽位已配置可用端点
- **THEN** 图片落工作区并登记 image 产物，`run.finished` 产物清单含该条目，支持图片的通道原生直发

#### Scenario: 未配置时工具不下发

- **WHEN** imageGen 槽位未配置（无端点/无凭据）
- **THEN** image_gen 不在工具表中，模型自然语言告知用户当前环境不支持生图

### Requirement: 生成护栏

image_gen MUST 受以下护栏约束：单次生成超时上限（默认 120s，超时中止并报错）；返回图片字节数上限（默认 20MB，超限拒绝）；同一会话单轮产物登记数沿用既有单轮产物上限。失败 MUST 返回结构化错误说明（超时/上游错误/解码失败三类），不产生半截产物文件。

#### Scenario: 上游超时

- **WHEN** 生图端点超过 120s 未返回
- **THEN** 工具中止并返回超时错误，工作区无半截文件，本轮收束无该产物

#### Scenario: 上游错误透出

- **WHEN** 生图端点返回 4xx/5xx
- **THEN** 工具返回包含上游状态码摘要的错误说明，模型可据实向用户解释
