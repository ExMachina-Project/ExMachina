//! 执行后端只读命令。依赖存在与后端已验证是两个字段，不允许混报。
use clap::Subcommand;
use exm_core::Core;

#[derive(Subcommand)]
pub(crate) enum BackendAction {
    /// 列出能力声明、就绪状态和依赖缺口
    List,
    /// 检查固定依赖；只执行 --version，不启动或更改任何后端
    Check { id: String },
}

pub(crate) async fn run(core: &Core, action: BackendAction, json: bool) -> anyhow::Result<()> {
    match action {
        BackendAction::List => {
            let backends = core.execution_backends().list();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schemaVersion": exm_core::exec_backend::EXEC_BACKEND_SCHEMA_VERSION,
                        "backends": backends,
                    }))?
                );
            } else {
                for backend in backends {
                    println!(
                        "{}  ready={}  isolated={}  {}",
                        backend.id,
                        backend.ready(),
                        backend.capabilities.isolated,
                        backend.readiness.detail()
                    );
                }
                println!("目录不是执行成功证据；详查：exm backend check <id>");
            }
        }
        BackendAction::Check { id } => {
            let check = core.check_execution_backend(&id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&check)?);
            } else {
                println!(
                    "{}  ready={}  dependencyAvailable={:?}\n{}",
                    check.backend.id, check.ready, check.dependency_available, check.detail
                );
            }
        }
    }
    Ok(())
}
