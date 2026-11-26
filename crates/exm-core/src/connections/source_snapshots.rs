//! 只读来源快照的合并与失效；不是来源配置库、登记服务或执行许可。
//!
//! 来源读取由调用方完成，本模块从不刷新远端。票据绑定一次本地读取代际，
//! 结果还须与权威登记逐条对账；晚到响应不能覆盖新登记，单来源失败不清空其他来源。

use super::capability::CapabilityDescriptor;
use super::identity::{
    ConnectionRef, ConnectionRegistration, RegistrationBinding, RegistrationRegistry,
    RegistrationState, SourceKind, SourceScope,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

const MAX_SOURCES: usize = 128;
const MAX_ROWS_PER_SOURCE: usize = 4096;
const MAX_CAPABILITIES_PER_ROW: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourcePartition {
    kind: SourceKind,
    scope: SourceScope,
}

impl SourcePartition {
    pub fn new(kind: SourceKind, scope: SourceScope) -> Self {
        Self { kind, scope }
    }
}

/// 只能由当前cache签发，不可从来源响应恢复或替换。
pub struct SourceReadTicket {
    partition: SourcePartition,
    sequence: u64,
    cache_id: uuid::Uuid,
}

/// 安全目录材料；不包含端点、命令、环境、凭据或原始错误正文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedConnection {
    pub registration: ConnectionRegistration,
    pub capabilities: Vec<CapabilityDescriptor>,
}

impl CachedConnection {
    fn validate(&self, partition: &SourcePartition, registry: &RegistrationRegistry) -> Result<()> {
        let source = self.registration.source();
        ensure!(
            source.kind() == partition.kind && source.scope() == &partition.scope,
            "来源快照范围不匹配"
        );
        ensure!(
            self.registration.state() != RegistrationState::Deleted
                && registry.get(self.registration.reference()) == Some(&self.registration),
            "来源响应的登记代际或版本已经失效"
        );
        ensure!(
            self.capabilities.len() <= MAX_CAPABILITIES_PER_ROW,
            "来源能力快照超过上限"
        );
        let mut ids = HashSet::new();
        for capability in &self.capabilities {
            capability.validate()?;
            ensure!(ids.insert(&capability.id), "来源能力ID重复");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFreshness {
    Fresh,
    Stale,
    Invalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceReadFailure {
    Unavailable,
    Forbidden,
    InvalidResult,
}

/// 内部管理快照仍须由目录层先ACL再分页；不能直接返回给Agent。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSourceRow {
    pub connection: CachedConnection,
    pub freshness: SnapshotFreshness,
    pub last_success_at_ms: u64,
    pub stale_since_ms: Option<u64>,
    pub source_revision: String,
}

#[derive(Default)]
struct SourceState {
    sequence: u64,
    rows: Vec<CachedSourceRow>,
    failure: Option<SourceReadFailure>,
}

pub struct SourceSnapshotCache {
    cache_id: uuid::Uuid,
    sources: HashMap<SourcePartition, SourceState>,
}

impl Default for SourceSnapshotCache {
    fn default() -> Self {
        Self {
            cache_id: uuid::Uuid::new_v4(),
            sources: HashMap::new(),
        }
    }
}

impl SourceSnapshotCache {
    pub fn begin_read(&mut self, partition: SourcePartition) -> Result<SourceReadTicket> {
        ensure!(
            self.sources.contains_key(&partition) || self.sources.len() < MAX_SOURCES,
            "来源快照数量超过上限"
        );
        let state = self.sources.entry(partition.clone()).or_default();
        state.sequence = state
            .sequence
            .checked_add(1)
            .context("来源读取代际已耗尽")?;
        Ok(SourceReadTicket {
            partition,
            sequence: state.sequence,
            cache_id: self.cache_id,
        })
    }

    fn state_for(&mut self, ticket: &SourceReadTicket) -> Result<&mut SourceState> {
        ensure!(ticket.cache_id == self.cache_id, "来源票据不属于当前缓存");
        let state = self
            .sources
            .get_mut(&ticket.partition)
            .context("来源票据无效")?;
        ensure!(
            state.sequence == ticket.sequence,
            "来源响应已被新读取代际替代"
        );
        Ok(state)
    }

    /// 成功批次先全量校验再替换，不允许部分坏数据留下半次更新。
    /// 权威来源确认删除的对象保留名称及原代际，并固定为invalidated。
    pub fn finish_success(
        &mut self,
        ticket: SourceReadTicket,
        source_revision: String,
        records: Vec<CachedConnection>,
        registry: &RegistrationRegistry,
        now_ms: u64,
    ) -> Result<()> {
        ensure!(now_ms > 0, "来源观察时间无效");
        ensure!(
            !source_revision.is_empty()
                && source_revision.len() <= 512
                && !source_revision
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()),
            "来源快照revision无效"
        );
        ensure!(records.len() <= MAX_ROWS_PER_SOURCE, "来源目录超过缓存上限");
        let mut seen = HashSet::new();
        for record in &records {
            record.validate(&ticket.partition, registry)?;
            ensure!(
                seen.insert(record.registration.reference().clone()),
                "来源对象重复"
            );
        }
        let state = self.state_for(&ticket)?;
        for record in &records {
            ensure!(
                !state.rows.iter().any(|row| {
                    row.freshness == SnapshotFreshness::Invalidated
                        && row.connection.registration.reference()
                            == record.registration.reference()
                        && row.connection.registration.revision() == record.registration.revision()
                }),
                "失效来源须重新登记或重验登记版本，不能恢复旧授权绑定"
            );
        }
        let mut next: Vec<_> = records
            .into_iter()
            .map(|connection| CachedSourceRow {
                connection,
                freshness: SnapshotFreshness::Fresh,
                last_success_at_ms: now_ms,
                stale_since_ms: None,
                source_revision: source_revision.clone(),
            })
            .collect();
        for old in &state.rows {
            if !seen.contains(old.connection.registration.reference()) {
                let mut row = old.clone();
                row.freshness = SnapshotFreshness::Invalidated;
                row.stale_since_ms.get_or_insert(now_ms);
                next.push(row);
            }
        }
        ensure!(
            next.len() <= MAX_ROWS_PER_SOURCE,
            "历史来源对象超过缓存上限"
        );
        state.rows = next;
        state.failure = None;
        Ok(())
    }

    pub fn finish_failure(
        &mut self,
        ticket: SourceReadTicket,
        failure: SourceReadFailure,
        now_ms: u64,
    ) -> Result<()> {
        ensure!(now_ms > 0, "来源观察时间无效");
        let state = self.state_for(&ticket)?;
        for row in &mut state.rows {
            if row.freshness != SnapshotFreshness::Invalidated {
                row.freshness = SnapshotFreshness::Stale;
                row.stale_since_ms.get_or_insert(now_ms);
            }
        }
        state.failure = Some(failure);
        Ok(())
    }

    /// 每次观察都与登记owner对账；重登记/禁用/来源版本变化立即失效旧快照。
    /// 陈旧或失效目录只可展示，不证明通信可用，更不能作为grant或批准。
    pub fn rows(
        &self,
        partition: &SourcePartition,
        registry: &RegistrationRegistry,
    ) -> Vec<CachedSourceRow> {
        self.sources.get(partition).map_or_else(Vec::new, |source| {
            source
                .rows
                .iter()
                .map(|row| {
                    let mut row = row.clone();
                    if registry.get(row.connection.registration.reference())
                        != Some(&row.connection.registration)
                        || row.connection.registration.state() == RegistrationState::Deleted
                    {
                        row.freshness = SnapshotFreshness::Invalidated;
                    }
                    row
                })
                .collect()
        })
    }

    pub fn failure(&self, partition: &SourcePartition) -> Option<SourceReadFailure> {
        self.sources
            .get(partition)
            .and_then(|source| source.failure)
    }

    /// 目录层可先用完整引用过滤私域，再做计数/分页，不能按显示名找对象。
    pub fn current_row(
        &self,
        partition: &SourcePartition,
        reference: &ConnectionRef,
        registry: &RegistrationRegistry,
    ) -> Option<CachedSourceRow> {
        self.rows(partition, registry)
            .into_iter()
            .find(|row| row.connection.registration.reference() == reference)
    }

    /// 仅证明已有绑定的来源快照仍新鲜、同版本且未失效；不是执行许可。
    /// 操作服务使用时仍须求交当前来源权限、grant、审批、根执行等所有准入。
    pub fn is_current_for_binding(
        &self,
        binding: &RegistrationBinding,
        registry: &RegistrationRegistry,
    ) -> bool {
        if registry.validate_binding(binding).is_err() {
            return false;
        }
        let partition =
            SourcePartition::new(binding.source().kind(), binding.source().scope().clone());
        self.current_row(&partition, binding.connection(), registry)
            .is_some_and(|row| {
                row.freshness == SnapshotFreshness::Fresh
                    && row.connection.registration.source() == binding.source()
                    && row.connection.registration.revision() == binding.registration_revision()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::identity::{ConnectionKind, SourceLifecycle, SourceRef};

    const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";

    fn register(
        registry: &mut RegistrationRegistry,
        authority: &str,
    ) -> (SourcePartition, ConnectionRef) {
        let scope =
            SourceScope::new(INSTANCE, authority, "account-a", Some("project-a".into())).unwrap();
        let partition = SourcePartition::new(SourceKind::Software, scope.clone());
        let source = SourceRef::new(
            SourceKind::Software,
            "document-api",
            scope,
            None,
            SourceLifecycle::known("generation-1").unwrap(),
            "revision-1",
        )
        .unwrap();
        let reference = registry
            .register(ConnectionKind::Software, source, "同名软件")
            .unwrap();
        registry.set_enabled(&reference, true).unwrap();
        (partition, reference)
    }

    fn record(registry: &RegistrationRegistry, reference: &ConnectionRef) -> CachedConnection {
        CachedConnection {
            registration: registry.get(reference).unwrap().clone(),
            capabilities: vec![],
        }
    }

    #[test]
    fn source_503_retains_only_its_stale_snapshot_and_does_not_zero_other_sources() {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (a, ref_a) = register(&mut registry, "service-a");
        let (b, ref_b) = register(&mut registry, "service-b");
        let mut cache = SourceSnapshotCache::default();
        for (partition, reference) in [(&a, &ref_a), (&b, &ref_b)] {
            let ticket = cache.begin_read(partition.clone()).unwrap();
            cache
                .finish_success(
                    ticket,
                    "revision-1".into(),
                    vec![record(&registry, reference)],
                    &registry,
                    100,
                )
                .unwrap();
        }
        // HTTP503等来源不可用只映射稳定错误，原始响应正文不进缓存。
        let ticket = cache.begin_read(a.clone()).unwrap();
        cache
            .finish_failure(ticket, SourceReadFailure::Unavailable, 200)
            .unwrap();
        let row = cache.current_row(&a, &ref_a, &registry).unwrap();
        assert_eq!(row.freshness, SnapshotFreshness::Stale);
        assert_eq!(row.last_success_at_ms, 100);
        assert_eq!(row.stale_since_ms, Some(200));
        assert_eq!(row.connection.registration.display_name(), "同名软件");
        assert_eq!(
            cache.current_row(&b, &ref_b, &registry).unwrap().freshness,
            SnapshotFreshness::Fresh
        );
        assert_eq!(cache.failure(&a), Some(SourceReadFailure::Unavailable));
        assert_eq!(cache.failure(&b), None);
        assert!(!cache.is_current_for_binding(&registry.binding_for(&ref_a).unwrap(), &registry));
        assert!(cache.is_current_for_binding(&registry.binding_for(&ref_b).unwrap(), &registry));
    }

    #[tokio::test]
    async fn real_loopback_503_keeps_last_success_without_caching_remote_error_text() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let router = axum::Router::new().route(
            "/catalog",
            axum::routing::get(move || {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "source-error-canary-secret",
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (a, ref_a) = register(&mut registry, "service-a");
        let (b, ref_b) = register(&mut registry, "service-b");
        let mut cache = SourceSnapshotCache::default();
        for (partition, reference) in [(&a, &ref_a), (&b, &ref_b)] {
            let ticket = cache.begin_read(partition.clone()).unwrap();
            cache
                .finish_success(
                    ticket,
                    "v1".into(),
                    vec![record(&registry, reference)],
                    &registry,
                    100,
                )
                .unwrap();
        }
        let ticket = cache.begin_read(a.clone()).unwrap();
        let response = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}/catalog"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        drop(response); // 原始远端错误正文不读取、不保存或转发。
        cache
            .finish_failure(ticket, SourceReadFailure::Unavailable, 200)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.current_row(&a, &ref_a, &registry).unwrap().freshness,
            SnapshotFreshness::Stale
        );
        assert_eq!(
            cache.current_row(&b, &ref_b, &registry).unwrap().freshness,
            SnapshotFreshness::Fresh
        );
        let serialized = serde_json::to_string(&cache.rows(&a, &registry)).unwrap();
        assert!(!serialized.contains("canary-secret"));
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn older_response_cannot_override_latest_read_or_new_registration_generation() {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (partition, reference) = register(&mut registry, "service-a");
        let old_binding = registry.binding_for(&reference).unwrap();
        let mut cache = SourceSnapshotCache::default();
        let first = cache.begin_read(partition.clone()).unwrap();
        cache
            .finish_success(
                first,
                "v1".into(),
                vec![record(&registry, &reference)],
                &registry,
                100,
            )
            .unwrap();
        let late = cache.begin_read(partition.clone()).unwrap();
        let old_data = record(&registry, &reference);
        registry.delete(&reference).unwrap();
        let replacement = registry
            .reregister(
                &reference,
                old_data.registration.source().clone(),
                "同名软件",
            )
            .unwrap();
        registry.set_enabled(&replacement, true).unwrap();
        assert!(registry.validate_binding(&old_binding).is_err());
        // 即使没有新的读取票据，旧响应也须与权威登记当前代际对账。
        assert!(cache
            .finish_success(late, "v1".into(), vec![old_data.clone()], &registry, 200)
            .is_err());
        let late = cache.begin_read(partition.clone()).unwrap();
        let latest = cache.begin_read(partition.clone()).unwrap();
        cache
            .finish_success(
                latest,
                "v2".into(),
                vec![record(&registry, &replacement)],
                &registry,
                300,
            )
            .unwrap();
        assert!(cache
            .finish_success(late, "v1".into(), vec![old_data], &registry, 400)
            .is_err());
        let old = cache
            .current_row(&partition, &reference, &registry)
            .unwrap();
        assert_eq!(old.freshness, SnapshotFreshness::Invalidated);
        assert_eq!(old.connection.registration.display_name(), "同名软件");
        assert_eq!(
            cache
                .current_row(&partition, &replacement, &registry)
                .unwrap()
                .source_revision,
            "v2"
        );
        assert!(registry.validate_binding(&old_binding).is_err());
    }

    #[test]
    fn malformed_batches_and_foreign_tickets_do_not_partially_commit() {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (partition, reference) = register(&mut registry, "service-a");
        let mut cache = SourceSnapshotCache::default();
        let ticket = cache.begin_read(partition.clone()).unwrap();
        let record = record(&registry, &reference);
        assert!(cache
            .finish_success(
                ticket,
                "v1".into(),
                vec![record.clone(), record],
                &registry,
                100
            )
            .is_err());
        assert!(cache.rows(&partition, &registry).is_empty());
        let ticket = cache.begin_read(partition.clone()).unwrap();
        let mut foreign = SourceSnapshotCache::default();
        foreign.begin_read(partition).unwrap();
        assert!(foreign
            .finish_failure(ticket, SourceReadFailure::InvalidResult, 100)
            .is_err());
    }

    #[test]
    fn disable_and_reenable_never_revalidate_a_cached_approval_target() {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (partition, reference) = register(&mut registry, "service-a");
        let binding = registry.binding_for(&reference).unwrap();
        let mut cache = SourceSnapshotCache::default();
        let ticket = cache.begin_read(partition.clone()).unwrap();
        cache
            .finish_success(
                ticket,
                "v1".into(),
                vec![record(&registry, &reference)],
                &registry,
                100,
            )
            .unwrap();
        registry.set_enabled(&reference, false).unwrap();
        registry.set_enabled(&reference, true).unwrap();
        assert_eq!(
            cache
                .current_row(&partition, &reference, &registry)
                .unwrap()
                .freshness,
            SnapshotFreshness::Invalidated
        );
        assert!(registry.validate_binding(&binding).is_err());
        assert!(!cache.is_current_for_binding(&binding, &registry));
        assert!(
            !cache.is_current_for_binding(&registry.binding_for(&reference).unwrap(), &registry)
        );
    }

    #[test]
    fn removed_source_retains_name_and_cannot_revive_the_same_registration_approval() {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (partition, reference) = register(&mut registry, "service-a");
        let binding = registry.binding_for(&reference).unwrap();
        let mut cache = SourceSnapshotCache::default();
        let ticket = cache.begin_read(partition.clone()).unwrap();
        cache
            .finish_success(
                ticket,
                "v1".into(),
                vec![record(&registry, &reference)],
                &registry,
                100,
            )
            .unwrap();
        let ticket = cache.begin_read(partition.clone()).unwrap();
        cache
            .finish_success(ticket, "v2".into(), vec![], &registry, 200)
            .unwrap();
        assert_eq!(
            cache
                .current_row(&partition, &reference, &registry)
                .unwrap()
                .connection
                .registration
                .display_name(),
            "同名软件"
        );
        assert!(!cache.is_current_for_binding(&binding, &registry));
        let ticket = cache.begin_read(partition.clone()).unwrap();
        assert!(cache
            .finish_success(
                ticket,
                "v3".into(),
                vec![record(&registry, &reference)],
                &registry,
                300
            )
            .is_err());
        assert!(!cache.is_current_for_binding(&binding, &registry));
        registry.set_enabled(&reference, false).unwrap();
        registry.set_enabled(&reference, true).unwrap();
        let ticket = cache.begin_read(partition).unwrap();
        cache
            .finish_success(
                ticket,
                "v4".into(),
                vec![record(&registry, &reference)],
                &registry,
                400,
            )
            .unwrap();
        assert!(registry.validate_binding(&binding).is_err());
        assert!(!cache.is_current_for_binding(&binding, &registry));
        assert!(cache.is_current_for_binding(&registry.binding_for(&reference).unwrap(), &registry));
    }

    #[test]
    fn source_revision_cas_invalidates_old_bindings_and_refuses_account_or_lifecycle_rebinding() {
        let mut registry = RegistrationRegistry::new(INSTANCE).unwrap();
        let (partition, reference) = register(&mut registry, "service-a");
        let binding = registry.binding_for(&reference).unwrap();
        let before = record(&registry, &reference);
        let source = before.registration.source();
        let updated = SourceRef::new(
            source.kind(),
            source.source_id(),
            source.scope().clone(),
            None,
            source.lifecycle().clone(),
            "revision-2",
        )
        .unwrap();
        let mut cache = SourceSnapshotCache::default();
        let ticket = cache.begin_read(partition.clone()).unwrap();
        cache
            .finish_success(
                ticket,
                "revision-1".into(),
                vec![before.clone()],
                &registry,
                100,
            )
            .unwrap();
        registry
            .observe_source_revision(&reference, before.registration.revision(), updated.clone())
            .unwrap();
        assert!(registry.validate_binding(&binding).is_err());
        assert_eq!(
            cache
                .current_row(&partition, &reference, &registry)
                .unwrap()
                .freshness,
            SnapshotFreshness::Invalidated
        );
        let current = registry.get(&reference).unwrap().clone();
        assert!(registry
            .observe_source_revision(&reference, before.registration.revision(), source.clone())
            .is_err());
        assert!(registry
            .observe_source_revision(
                &reference,
                current.revision(),
                SourceRef::new(
                    source.kind(),
                    source.source_id(),
                    source.scope().clone(),
                    None,
                    SourceLifecycle::known("recreated-object").unwrap(),
                    "revision-3"
                )
                .unwrap()
            )
            .is_err());
        let scope = source.scope();
        assert!(registry
            .observe_source_revision(
                &reference,
                current.revision(),
                SourceRef::new(
                    source.kind(),
                    source.source_id(),
                    SourceScope::new(
                        scope.instance_id(),
                        scope.authority_id(),
                        "account-b",
                        scope.project_id().map(str::to_owned)
                    )
                    .unwrap(),
                    None,
                    source.lifecycle().clone(),
                    "revision-3"
                )
                .unwrap()
            )
            .is_err());
        registry
            .observe_source_revision(&reference, current.revision(), updated)
            .unwrap();
        assert_eq!(
            registry.get(&reference).unwrap(),
            &current,
            "相同revision观察幂等"
        );
        assert!(registry.validate_binding(&binding).is_err());
    }
}
