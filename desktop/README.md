# EXMACHINA 桌面端（Tauri v2）

桌面端是独立的编程工作区：React + TypeScript + Vite + antd，使用 CodeMirror 编辑代码。
左侧管理会话，中间在对话和文件标签间切换，右侧展示任务与执行过程；文件树、变更和 Git 有独立入口。
项目、连接、草稿、会话和编辑器状态由桌面工作区隔离管理。实例级模型连接可在桌面设置中维护；完整后台管理与监控仍由 WebUI 负责。
桌面源码在 `src/`，构建到 `dist/` 后由 Tauri 嵌入二进制。
捆绑的 `exm-gateway` 随应用启动充当后端（REST / WS 同端口）；
同时支持 `--remote` 直连远程网关（服务器上的后台服务）。

> 模型连接可在桌面端「实例配置」中维护；组与个体、通道、任务和安全策略仍在 WebUI 管理，桌面端不会打包 WebUI 管理页。
> 独立移动应用通过 IP/域名与密钥连接桌面或无头网关（移动应用仍在开发中）；浏览器打开网关地址进入 WebUI 管理台。局域网分享必须配合访问密钥，并建议使用 HTTPS/VPN。

## 协作群聊

在桌面打开一个项目，选择「协作群聊 → 新建协作群聊」。从**同一连接**下的项目中选 2–8 位成员，每位绑定一个项目目录和一个独立智能体或现有组。群聊保存在创建时的项目下，成员名单固定。

每轮勾选收件人，再写公共消息或逐个分配任务。回复、执行状态和待审批都显示在同一个窗口；不打算转发的回复不会自动进入其他成员的上下文。需要交接时，点「引用到草稿」，确认引用内容和收件人后再发。

不同目录的成员可同时执行；同一轮内绑定相同目录的成员串行执行。切换项目或退出远程桌面客户端不会取消已受理的任务；停止不会撤销已经完成的操作。网关崩溃后未收束的任务标记为「已中断」，不会自动重跑。

需要网关声明 `collaboration-v1`。当前不跨网关、不支持群聊附件输入，收束后显示每位成员的回复；普通单聊和组对话不受影响。

## 启动形态

| 形态 | 命令 | 行为 |
|------|------|------|
| 本机模式 | `exmachina-desktop` | 拉起捆绑网关（空闲端口 + 用户数据目录），主窗口进入编程工作区 |
| 远程模式 | `exmachina-desktop --remote http://host:4173 --key <authKey>` | 主窗口对话界面直连远程网关（服务器上的后台服务） |
| 带密钥 | `--key <authKey>` | 密钥经初始化脚本注入 `window.__EXM_GATEWAY__`，免登录门 |

## 构建步骤

前置：Rust stable、Node ≥ 22、平台 WebView 运行时（Windows: WebView2 预装于 Win10/11）。

```bash
# 1. 在仓库根安装前端依赖，并构建网关
npm ci
cargo build -p exm-gateway --release

# 2. 放置捆绑网关（桌面使用独立构建产物）
mkdir -p desktop/src-tauri/binaries
cp target/release/exm-gateway.exe desktop/src-tauri/binaries/     # Windows
# cp target/release/exm-gateway desktop/src-tauri/binaries/       # Linux / macOS
# 如需桌面设置中的「打开实例控制台」，构建并部署对应的 webui/dist。

# 3. 安装依赖并构建安装包
cd desktop
npm ci
npm run tauri build
```

产物：`desktop/src-tauri/target/release/bundle/` 下的 NSIS 安装包（Windows）/ AppImage（Linux）/ DMG（macOS）。

前端检查：在仓库根执行 `npm run check:desktop-ui`、`npm run build:desktop-ui`。
原生开发：`npm --prefix desktop run dev` 自动启动桌面 Vite 服务并使用热更新。
浏览器调试桌面界面：`npm --prefix desktop run dev:ui`，在「项目与连接」中添加网关地址、访问密钥和网关上的项目目录；这不是网关托管的 WebUI 管理台。

## 设计说明

- **壳只做两件事**：拉起/连接网关 + 提供窗口。全部业务仍在网关进程（单进程交付原则不变）。
- **生命周期**：应用退出时终止网关子进程；网关崩溃显示恢复页，由用户一键重启同端口网关并自动重连。
- **数据隔离**：本机模式网关使用用户数据目录（`EXM_DATA_DIR`），与仓库工作区解耦；
  默认工作区为用户数据目录内的 `workspace/`，可在桌面标题栏打开项目。
- **项目切换**：连接目录绑定到会话和请求作用域；本地模式使用原生目录选择，远程模式输入网关机器上的目录。未保存文件按项目保留，退出前统一提示。
- **编辑与审查**：相对路径受到工作区校验，二进制和超过 200KB 的文件只读；保存携带原文与工作区，冲突时保留本地修改。
  Git 丢弃、硬重置与推送先生成审批，审批绑定创建时的项目与参数；未初始化提交的仓库支持暂存和取消暂存。
- **远程模式零捆绑**：`--remote` 时不启动本地进程，等于一个连服务器的瘦客户端。
- **连接恢复**：空闲会话通过 `ping` / `pong` 保活，重新连上时校准消息、运行态和待审批；跨会话审批显示来源并提供跳转。
- **输入保护**：文字草稿与图片按会话隔离，切到代码页仍保留；发送失败可直接重试。文字在当前窗口的 `sessionStorage` 内保存，图片仅在内存中保留。

桌面页面回归：在仓库根目录执行 `npm run test:ui`。原生壳单测：`cargo test --manifest-path desktop/src-tauri/Cargo.toml`。
其中网关进程恢复集成用例需要 `EXM_GATEWAY_BIN` 指向已构建的网关二进制；未设置时该用例跳过执行实际进程。
