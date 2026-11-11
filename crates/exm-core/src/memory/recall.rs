//! memory 域分文件（自 memory.rs 按域拆出，纯搬移零行为变更）。

use super::*;

impl MemoryStore {
    /// 群体记忆检索：仅共享条目（指挥体规划用，看不到任何个体私有记忆）。
    /// `group` 为 Some(gid) 时只返回「全局条目 + 该组条目」，其他组条目不可见。
    pub fn recall(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&str>,
        group: Option<&str>,
        qe: Option<&[f32]>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        self.recall_filtered(query, limit, scope, AgentLayer::SharedOnly, group, qe)
    }

    /// 全量检索（管理视角：群体 + 所有个体私有；组维度按 `group` 过滤）
    pub fn recall_all(
        &self,
        query: &str,
        limit: usize,
        group: Option<&str>,
        qe: Option<&[f32]>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        self.recall_filtered(query, limit, None, AgentLayer::All, group, qe)
    }

    /// 个体检索：该智能体的私有记忆（agent_id 匹配）+ 群体记忆，个体条目加成
    pub fn recall_for_agent(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
        group: Option<&str>,
        qe: Option<&[f32]>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        self.recall_filtered(query, limit, None, AgentLayer::Agent(agent_id), group, qe)
    }

    #[allow(clippy::too_many_arguments)]
    fn recall_filtered(
        &self,
        query: &str,
        limit: usize,
        scope: Option<&str>,
        layer: AgentLayer<'_>,
        group: Option<&str>,
        qe: Option<&[f32]>,
    ) -> Result<Vec<RecallHit>> {
        let q_tokens = tokens(query);
        if q_tokens.is_empty() {
            // 空查询（无有效词项）回退「近期列表」：回退不是免检通道——
            // 分层（个体/群体）与 scope 过滤必须照常生效，
            // 否则个体视角的空查询会把他人私有记忆当「近期」召回（越权泄漏）。
            return self.recent_layered(limit, group, layer, scope);
        }

        // 候选：命中任一查询词项的条目 + 命中权重（附词项明细，调试视图 8.4 用）
        let mut overlap: std::collections::HashMap<
            String,
            (f64, usize, std::collections::BTreeMap<String, f64>),
        > = Default::default();
        for t in &q_tokens {
            let bucket: TermBucket = self.db.read_bucket_map(TERMS, t)?;
            if let Some(list) = bucket.get(t) {
                for (id, w) in list {
                    let slot = overlap
                        .entry(id.clone())
                        .or_insert((0.0, 0, Default::default()));
                    slot.0 += *w;
                    slot.1 += 1;
                    *slot.2.entry(t.clone()).or_insert(0.0) += *w;
                }
            }
        }
        if overlap.is_empty() {
            // 语义兜底：词项无命中但查询向量可用时，扫描已嵌入条目取相似 Top
            if qe.is_some() {
                let mut sem: Vec<RecallHit> = Vec::new();
                for e in self.db.list::<MemoryEntry>(ENTRIES)?.into_iter() {
                    if let Some(g) = group {
                        if let Some(eg) = &e.group_id {
                            if eg != g {
                                continue;
                            }
                        }
                    }
                    if !matches!(layer, AgentLayer::All) {
                        if let AgentLayer::Agent(a) = layer {
                            if e.agent_id.as_deref().is_some_and(|x| x != a) {
                                continue;
                            }
                        } else if e.agent_id.is_some() {
                            continue; // SharedOnly
                        }
                    }
                    let Some(sim) = e
                        .embedding
                        .as_deref()
                        .and_then(|v| Self::cosine(v, qe.unwrap()))
                    else {
                        continue;
                    };
                    if sim < 0.15 {
                        continue;
                    }
                    sem.push(RecallHit {
                        entry: e,
                        score: sim * 3.0,
                        reasons: vec![format!("语义相似 {:.0}%", (sim * 100.0) as u32)],
                        matched_terms: None,
                        semantic_score: Some((sim * 1000.0).round() / 1000.0),
                    });
                }
                sem.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                sem.truncate(limit);
                if !sem.is_empty() {
                    return Ok(sem);
                }
            }
            return self.recent_layered(limit, group, layer, scope);
        }
        let max_overlap = overlap.values().map(|v| v.0).fold(1.0f64, f64::max);

        let mut results: Vec<RecallHit> = Vec::new();
        for (id, (ov, matched, term_detail)) in overlap {
            let Some(entry) = self.get_entry(&id)? else {
                continue;
            };
            if let Some(sc) = scope {
                if entry.scope != sc && entry.scope != "global" && entry.scope != "project" {
                    continue;
                }
            }
            // 组隔离过滤（docs/09 §6）：全局条目处处可见；组条目仅组内可见
            if let Some(g) = group {
                if entry.group_id.as_deref().is_some_and(|eg| eg != g) {
                    continue;
                }
            }
            // 记忆分层过滤（docs/08）：个体 = 归属某智能体的私有记忆；群体 = 无归属共享
            let individual = entry.agent_id.is_some();
            let visible = match layer {
                AgentLayer::All => true,
                AgentLayer::SharedOnly => !individual,
                AgentLayer::Agent(owner) => !individual || entry.agent_id.as_deref() == Some(owner),
            };
            if !visible {
                continue;
            }
            let overlap_ratio = ov / max_overlap;
            let coverage = matched as f64 / q_tokens.len() as f64;
            let recency = 0.5f64.powf(age_days(&entry.updated_at) / 30.0);
            let usage = (entry.access_count as f64 / 10.0).min(1.0);
            let pinned_bonus = if entry.pinned { 0.5 } else { 0.0 };
            let layer_bonus = if individual { 0.3 } else { 0.0 };

            // 混合打分：词项信号 + 语义信号（向量与查询均可用时，70/30 融合）
            let sem = entry
                .embedding
                .as_deref()
                .and_then(|v| qe.and_then(|q| Self::cosine(v, q)));
            let score = 3.0 * overlap_ratio
                + 1.2 * coverage
                + 1.5 * entry.importance
                + 1.0 * recency
                + 0.4 * usage
                + entry.kind.boost()
                + pinned_bonus
                + layer_bonus
                + sem.map(|s| 2.0 * s).unwrap_or(0.0);

            let mut reasons = vec![
                format!("词项覆盖 {:.0}%", coverage * 100.0),
                format!("重要性 {:.2}", entry.importance),
                format!("时间衰减 {:.2}", recency),
                if individual {
                    "个体记忆".to_string()
                } else {
                    "群体记忆".to_string()
                },
            ];
            if let Some(s) = sem {
                reasons.push(format!("语义相似 {:.0}%", (s * 100.0) as u32));
            }
            if let Some(g) = &entry.group_id {
                reasons.push(format!("组范围 {g}"));
            }
            if entry.pinned {
                reasons.push("固定记忆".into());
            }
            results.push(RecallHit {
                entry,
                score,
                reasons,
                // 调试视图（8.4）：词项命中明细 + 语义分数数值
                matched_terms: Some(term_detail),
                semantic_score: sem.map(|s| (s * 1000.0).round() / 1000.0),
            });
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        for h in &results {
            let mut e = h.entry.clone();
            e.access_count += 1;
            e.last_accessed_at = Some(now_iso());
            let _ = self.save_entry(&e);
        }
        Ok(results)
    }

    pub fn recent(&self, limit: usize, group: Option<&str>) -> Result<Vec<RecallHit>> {
        self.recent_layered(limit, group, AgentLayer::All, None)
    }

    /// 分层版近期列表：`agent` 为 Some = 个体视角（该个体私有 + 群体共享）；None = 全量（管理视角）。
    /// 工具面的空查询列示走这里——「列示」与「检索」适用同一套分层口径。
    pub fn recent_in_layer(
        &self,
        agent: Option<&str>,
        limit: usize,
        group: Option<&str>,
    ) -> Result<Vec<RecallHit>> {
        let layer = match agent {
            Some(a) => AgentLayer::Agent(a),
            None => AgentLayer::All,
        };
        self.recent_layered(limit, group, layer, None)
    }

    /// 近期列表实现（分层 + scope + 组三维过滤，排序口径与历史版本一致）
    fn recent_layered(
        &self,
        limit: usize,
        group: Option<&str>,
        layer: AgentLayer<'_>,
        scope: Option<&str>,
    ) -> Result<Vec<RecallHit>> {
        let mut entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        if let Some(g) = group {
            entries.retain(|e| e.group_id.as_deref().is_none_or(|eg| eg == g));
        }
        entries.retain(|e| match layer {
            AgentLayer::All => true,
            AgentLayer::SharedOnly => e.agent_id.is_none(),
            AgentLayer::Agent(a) => e.agent_id.as_deref().is_none_or(|x| x == a),
        });
        // scope 口径与词项召回路径一致：命中请求 scope 或全局/项目级条目才可见
        if let Some(sc) = scope {
            entries.retain(|e| e.scope == sc || e.scope == "global" || e.scope == "project");
        }
        entries.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(
                    b.importance
                        .partial_cmp(&a.importance)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(b.updated_at.cmp(&a.updated_at))
        });
        entries.truncate(limit);
        Ok(entries
            .into_iter()
            .map(|e| {
                let score = e.importance;
                RecallHit {
                    entry: e,
                    score,
                    reasons: vec!["按重要性/时间排序".into()],
                    matched_terms: None,
                    semantic_score: None,
                }
            })
            .collect())
    }

    pub fn list(&self, kind: Option<MemoryKind>, limit: usize) -> Result<Vec<MemoryEntry>> {
        self.list_filtered(kind, None, limit, None)
    }

    /// 列表过滤：`agent` 为 Some(id) 时返回该个体私有 + 群体共享；`group` 过滤组范围；均 None 返回全部
    pub fn list_filtered(
        &self,
        kind: Option<MemoryKind>,
        agent: Option<&str>,
        limit: usize,
        group: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let mut entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        if let Some(k) = kind {
            entries.retain(|e| e.kind == k);
        }
        if let Some(id) = agent {
            entries.retain(|e| e.agent_id.as_deref() == Some(id) || e.agent_id.is_none());
        }
        if let Some(g) = group {
            entries.retain(|e| e.group_id.as_deref().is_none_or(|eg| eg == g));
        }
        entries.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(
                    b.importance
                        .partial_cmp(&a.importance)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(b.updated_at.cmp(&a.updated_at))
        });
        entries.truncate(limit);
        Ok(entries)
    }

    pub fn stats(&self) -> Result<serde_json::Value> {
        let entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        let mut by_kind = serde_json::Map::new();
        for k in MemoryKind::all() {
            let n = entries.iter().filter(|e| e.kind == k).count();
            if n > 0 {
                by_kind.insert(k.key().to_string(), serde_json::json!(n));
            }
        }
        let pinned = entries.iter().filter(|e| e.pinned).count();
        let individual = entries.iter().filter(|e| e.agent_id.is_some()).count();
        let shared = entries.len() - individual;
        let mut by_group: std::collections::BTreeMap<String, usize> = Default::default();
        for e in entries.iter().filter_map(|e| e.group_id.as_ref()) {
            *by_group.entry(e.clone()).or_default() += 1;
        }
        Ok(serde_json::json!({
            "total": entries.len(),
            "pinned": pinned,
            "individual": individual,
            "shared": shared,
            "groupScoped": entries.iter().filter(|e| e.group_id.is_some()).count(),
            "byGroup": by_group,
            "terms": self.db.bucket_key_count(TERMS)?,
            "indexBuckets": crate::fsdb::TERM_BUCKETS,
            "byKind": by_kind,
            "dbPath": self.root(),
        }))
    }

    // ---------------- 个体统计（进化反馈） ----------------

    pub fn record_agent_outcome(
        &self,
        agent_id: &str,
        status: &str,
        confidence: f64,
    ) -> Result<()> {
        let mut stat: AgentStat = self
            .db
            .get::<AgentStat>(STATS, agent_id)?
            .unwrap_or_else(|| AgentStat {
                agent_id: agent_id.to_string(),
                runs: 0,
                done: 0,
                blocked: 0,
                failed: 0,
                avg_confidence: 0.0,
                updated_at: now_iso(),
            });
        let total_conf = stat.avg_confidence * stat.runs as f64 + confidence;
        stat.runs += 1;
        match status {
            "done" => stat.done += 1,
            "blocked" => stat.blocked += 1,
            _ => stat.failed += 1,
        }
        stat.avg_confidence = total_conf / stat.runs as f64;
        stat.updated_at = now_iso();
        self.db.put(STATS, agent_id, &stat)
    }

    pub fn agent_stats(&self, limit: usize) -> Result<Vec<AgentStat>> {
        let mut list: Vec<AgentStat> = self.db.list(STATS)?;
        list.sort_by_key(|a| std::cmp::Reverse(a.runs));
        list.truncate(limit);
        Ok(list)
    }

    // ---------------- memory.md 渲染（基础记忆） ----------------

}
