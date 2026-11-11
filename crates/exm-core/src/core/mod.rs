//! core/ —— Core 领域拆分（codebase-refactor）：`impl Core` 按域分布在本目录，
//! lib.rs 保留组合根（struct 定义/构造/chat 粘合/emit/编排器构建）。
//! 模块地图见 lib.rs 的 Core struct doc 注释「方法分布图」。

pub(crate) mod approvals;
pub(crate) mod identity;
pub(crate) mod memory;
pub(crate) mod sessions;
pub(crate) mod skills;

#[cfg(test)]
mod tests;
