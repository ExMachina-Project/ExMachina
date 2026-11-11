//! core/plugins.rs —— Core 插件域（codebase-refactor 拆分）：插件管理器访问、
//! 安装/更新/卸载（plugin-ecosystem；CLI 与 REST 共用面）。纯搬移：方法签名与实现零改动。

use super::super::Core;
use std::sync::Arc;

impl Core {
    /// 插件管理器（注册面/管理端点共用）
    pub fn plugins(&self) -> Arc<crate::plugin::PluginManager> {
        self.plugins.read().clone()
    }
    /// 安装插件：staging 校验 → 落盘 → 热装载 → installation 来源持久化 →
    /// 声明式工具回接 ToolGateway。幂等冲突返回 Err（overwrite=true 走更新语义）。
    pub async fn plugin_install(
        &self,
        source: &crate::plugin::install::InstallSource,
        overwrite: bool,
    ) -> anyhow::Result<crate::plugin::install::InstallOutcome> {
        let plugins_dir = self.config().workspace_root.join("plugins");
        let pm = self.plugins();
        let host = crate::plugin::host_version_pub();
        let out = crate::plugin::install::install_from_source(
            &pm,
            source,
            &plugins_dir,
            &host,
            overwrite,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        // 来源记录 + 声明式工具回接
        let mut cfg = (*self.config()).clone();
        cfg.plugins
            .installations
            .insert(out.id.clone(), source.to_installation());
        self.apply_config(cfg)?;
        Ok(out)
    }
    /// 更新插件：来源取 installation 记录（缺记录报无法更新）；已是最新不落盘；
    /// 更新保 data/ 与启用状态。返回 (是否更新, 消息)。
    pub async fn plugin_update(&self, id: &str) -> anyhow::Result<(bool, String)> {
        let cfg = self.config();
        let Some(inst) = cfg.plugins.installations.get(id).cloned() else {
            anyhow::bail!("插件 {id} 无安装来源记录（手工拷入的插件无法自动更新）");
        };
        let source = crate::plugin::install::installation_to_source(&inst);
        let plugins_dir = cfg.workspace_root.join("plugins");
        let pm = self.plugins();
        let host = crate::plugin::host_version_pub();
        // 本地版本
        let local_v = pm
            .get(id)
            .map(|e| e.manifest.version.clone())
            .unwrap_or_default();
        // 来源版本探测（staging 只读，不落盘）
        let remote_m = crate::plugin::install::probe_source_version(&source, &host)
            .await
            .map_err(anyhow::Error::msg)?;
        let remote_v = remote_m.version.trim().to_string();
        if !remote_v.is_empty()
            && !local_v.trim().is_empty()
            && !crate::plugin::version_newer(&remote_v, &local_v)
        {
            return Ok((false, format!("{id} 已是最新（{local_v}）")));
        }
        // 记录更新前启用状态（更新后保持）
        let was_enabled = pm.get(id).map(|e| e.enabled).unwrap_or(true);
        crate::plugin::install::update_from_source(&pm, &source, &plugins_dir, &host)
            .await
            .map_err(anyhow::Error::msg)?;
        // 启用状态保持 + 持久化
        pm.set_enabled(id, was_enabled);
        let mut cfg = (*self.config()).clone();
        cfg.plugins.state.insert(id.to_string(), was_enabled);
        cfg.plugins.installations.insert(id.to_string(), inst);
        self.apply_config(cfg)?;
        Ok((true, format!("{id} 已更新到 {remote_v}（原 {local_v}）")))
    }
    /// 卸载插件（与 REST DELETE 同语义，CLI 用）：禁用 → 终止进程（provider 摘除）→
    /// 删目录 → 清 config 状态/来源/凭据。
    pub fn plugin_uninstall(&self, id: &str) -> anyhow::Result<bool> {
        let pm = self.plugins();
        let Some(e) = pm.get(id) else {
            anyhow::bail!("插件不存在：{id}");
        };
        if e.form == "builtin" || e.form == "virtual" {
            anyhow::bail!("{} 插件随宿主发布或收编，不可卸载（可禁用）", e.form);
        }
        let Some(dir) = e.dir.clone() else {
            anyhow::bail!("插件目录未知，无法卸载");
        };
        let plugins_root = self.config().workspace_root.join("plugins");
        if !dir.starts_with(&plugins_root) || dir == plugins_root {
            anyhow::bail!("插件目录越界，拒绝卸载");
        }
        pm.set_enabled(id, false);
        pm.remove_provider(id);
        pm.remove_entry(id);
        std::fs::remove_dir_all(&dir)?;
        let mut cfg = (*self.config()).clone();
        cfg.plugins.state.remove(id);
        cfg.plugins.installations.remove(id);
        cfg.plugins.credentials.remove(id);
        self.apply_config(cfg)?;
        Ok(true)
    }

}
