//! 统一连接领域的身份与能力基础。
//!
//! 当前仅提供契约、登记与只读投影，不代表适配器、操作服务或外部成员执行已就绪。
//! 来源配置/凭据仍由原服务管理；连接、能力声明和关系都不是执行授权。

pub mod capability;
pub mod catalog;
pub mod identity;
pub mod operation_state;
pub(crate) mod projection;
pub mod registration_store;
pub mod relations;
pub mod source_snapshots;
