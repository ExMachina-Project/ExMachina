//! memory 域分文件（自 memory.rs 按域拆出，纯搬移零行为变更）。
//! render 域：memory.md 渲染（分层/个体/组）与提示块渲染 + 自由函数族。

use super::*;

impl MemoryStore {
    pub fn render_memory_md(&self, path: impl AsRef<Path>) -> Result<usize> {
        self.render_memory_md_scoped(path, None)
    }

    /// 按记忆命名空间渲染（`None` = 全量管理视图）。
    /// 隔离分区只能看到「全局条目 + 本分区条目」，不得把其他组/分区的记忆写进人读视图。
    pub fn render_memory_md_scoped(
        &self,
        path: impl AsRef<Path>,
        group: Option<&str>,
    ) -> Result<usize> {
        let mut entries: Vec<MemoryEntry> = self.db.list(ENTRIES)?;
        if let Some(g) = group {
            entries.retain(|e| e.group_id.as_deref().is_none_or(|eg| eg == g));
        }
        entries.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let pinned: Vec<&MemoryEntry> = entries.iter().filter(|e| e.pinned).collect();
        let important: Vec<&MemoryEntry> = entries.iter().filter(|e| !e.pinned).take(20).collect();

        let mut md = String::new();
        md.push_str("# EXMACHINA 基础记忆\n\n");
        md.push_str("<!-- 本文件由 `exm memory render` 自动生成，是深层记忆库的人读视图；请勿手工编辑 -->\n\n");
        md.push_str(&format!("- 更新时间：{}\n", now_iso()));
        md.push_str(&format!("- 记忆库：`{}`\n", self.root()));
        md.push_str(&format!(
            "- 固定记忆 {} 条；下方另列高重要性记忆 {} 条\n\n",
            pinned.len(),
            important.len()
        ));

        md.push_str("## 固定记忆（核心）\n\n");
        if pinned.is_empty() {
            md.push_str("_（暂无；用 `exm memory add --pin` 或 `exm memory pin <id>` 添加）_\n\n");
        }
        for e in &pinned {
            md.push_str(&render_entry_md(e));
        }

        md.push_str("## 近期重要记忆\n\n");
        if important.is_empty() {
            md.push_str("_（暂无）_\n\n");
        }
        for e in &important {
            md.push_str(&render_entry_md(e));
        }

        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, md)?;
        Ok(pinned.len())
    }

    /// 个体浅层记忆视图：`entities/agents/<id>/MEMORY.md` = 该个体私有记忆 + 群体置顶（浅层，容量有限）。
    /// 深层记忆始终在 MemoryStore（数据库检索/衰减/压缩），此处只是人读快照。
    pub fn render_agent_memory_md(
        &self,
        agent: &str,
        group: Option<&str>,
        path: impl AsRef<Path>,
    ) -> Result<usize> {
        let mut entries: Vec<MemoryEntry> = self.list_filtered(None, Some(agent), 200, group)?;
        // 该个体的私有记忆在前（新→旧），群体共享仅收录置顶
        entries.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.created_at.cmp(&a.created_at))
        });
        let private: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| e.agent_id.as_deref() == Some(agent))
            .collect();
        let pinned_shared: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| e.agent_id.is_none() && e.pinned)
            .take(10)
            .collect();

        let mut md = String::new();
        md.push_str(&format!("# MEMORY · {agent}\n\n"));
        md.push_str("<!-- 浅层记忆（自动渲染的人读快照，容量有限）：深层记忆在记忆库（检索/衰减/压缩），编辑请走记忆面板或 exm memory -->\n\n");
        md.push_str(&format!("- 更新时间：{}\n", now_iso()));
        md.push_str(&format!(
            "- 私有记忆 {} 条；群体置顶 {} 条\n\n",
            private.len(),
            pinned_shared.len()
        ));

        md.push_str("## 个体私有记忆（浅层）\n\n");
        if private.is_empty() {
            md.push_str("_（暂无；受阻教训与个体经验沉淀在深层记忆库，可经检索召回）_\n\n");
        }
        for e in private.iter().take(50) {
            md.push_str(&render_entry_md(e));
        }

        md.push_str("## 群体置顶（共享核心）\n\n");
        if pinned_shared.is_empty() {
            md.push_str("_（暂无）_\n\n");
        }
        for e in &pinned_shared {
            md.push_str(&render_entry_md(e));
        }

        let md = truncate_shallow(md);
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, md)?;
        Ok(private.len())
    }

    /// 组浅层记忆视图：`groups/<gid>/MEMORY.md` = 组域共享记忆（浅层，容量有限）。
    /// 深层记忆在 MemoryStore；子个体不单独建文件，成员记忆经检索注入。
    pub fn render_group_memory_md(&self, gid: &str, path: impl AsRef<Path>) -> Result<usize> {
        let entries: Vec<MemoryEntry> = self.list_filtered(None, None, 200, Some(gid))?;
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| {
            b.pinned.cmp(&a.pinned).then(
                b.importance
                    .partial_cmp(&a.importance)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        let pinned: Vec<&MemoryEntry> = sorted.iter().filter(|e| e.pinned).take(10).collect();
        let recent: Vec<&MemoryEntry> = sorted.iter().filter(|e| !e.pinned).take(20).collect();

        let mut md = String::new();
        md.push_str(&format!("# MEMORY · {gid}\n\n"));
        md.push_str("<!-- 组浅层记忆（自动渲染的人读快照，容量有限）：深层记忆在记忆库（检索/衰减/压缩），编辑请走记忆面板或 exm memory -->\n\n");
        md.push_str(&format!("- 更新时间：{}\n", now_iso()));
        md.push_str(&format!(
            "- 组域记忆 {} 条（置顶 {}）\n\n",
            entries.len(),
            pinned.len()
        ));

        md.push_str("## 置顶（核心）\n\n");
        if pinned.is_empty() {
            md.push_str("_（暂无）_\n\n");
        }
        for e in &pinned {
            md.push_str(&render_entry_md(e));
        }

        md.push_str("## 近期重要\n\n");
        if recent.is_empty() {
            md.push_str("_（暂无）_\n\n");
        }
        for e in &recent {
            md.push_str(&render_entry_md(e));
        }

        let md = truncate_shallow(md);
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, md)?;
        Ok(pinned.len())
    }

    /// 生成注入给指挥体的「历史记忆」提示块
    pub fn render_prompt_block(hits: &[RecallHit]) -> String {
        if hits.is_empty() {
            return String::new();
        }
        let mut s = String::from("## 历史记忆（群体记忆库召回，供参考，不得当事实直接引用）\n");
        for h in hits {
            s.push_str(&format!(
                "- [{}|{:.2}] {}：{}（来源：{}）\n",
                h.entry.kind.key(),
                h.score,
                h.entry.title,
                h.entry.body.chars().take(200).collect::<String>(),
                h.entry
                    .source_ref
                    .clone()
                    .unwrap_or_else(|| "本机记忆库".into())
            ));
        }
        s
    }
}

// ---------------------------------------------------------------- 工具函数

/// 衰减因子 = 0.5^(age / half_life)。
/// 边界自洽（逐条钳制的原因）：
/// - `half_life ≤ 0 / NaN`：无数学意义的半衰期，按 1 天处理（`f64::max` 恰好也是这个语义，
///   与历史行为一致）；配置层另有清洗（非法值回落默认 30 天），此处是最后一道兜底。
/// - `age < 0`（未来时间戳，时钟回拨/脏数据）：按 0 处理——衰减只能让记忆变淡，
///   绝不能反向「增寿」，否则每次衰减都在抬升重要性，重复整理会无限膨胀。
/// - `half_life` 极大（→∞）：age/half_life → 0，因子 → 1，等于不衰减（符合直觉：
///   「半衰期无穷长」就是「永不遗忘」）。
pub(crate) fn decay_factor(age_days: f64, half_life_days: f64) -> f64 {
    let hl = if half_life_days.is_finite() && half_life_days > 0.0 {
        half_life_days
    } else {
        1.0
    };
    0.5f64.powf(age_days.max(0.0) / hl)
}

/// 衰减后重要性：乘因子后收进 [floor, 1.0]。
/// 下限保底：濒死记忆缓慢趋近 floor 而非骤归零（floor 语义 = 衰减底线）；
/// 上限钳 1.0：floor 传入越界值（>1）时也不得把重要性抬出 [0,1] 合法域。
pub(crate) fn decayed_importance(current: f64, age_days: f64, half_life_days: f64, floor: f64) -> f64 {
    let floor = floor.clamp(0.0, 1.0);
    (current * decay_factor(age_days, half_life_days)).clamp(floor, 1.0)
}

/// 按字符截断（非字节）：中文场景按字节切会劈开 UTF-8 序列产生乱码尾巴
pub(crate) fn cap_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        s.chars().take(cap).collect()
    }
}

pub(crate) fn render_entry_md(e: &MemoryEntry) -> String {
    let tags = if e.tags.is_empty() {
        String::new()
    } else {
        format!(" `{}`", e.tags.join("` `"))
    };
    format!(
        "- **{}**（{}｜重要性 {:.2}{}）\n  {}\n",
        e.title,
        e.kind.key(),
        e.importance,
        tags,
        e.body.replace('\n', " ")
    )
}

/// 浅层记忆容量上限（字符）：MEMORY.md 是人读快照，不允许膨胀——超出即截断（深层记忆在记忆库）
const SHALLOW_CAP_CHARS: usize = 6000;

pub(crate) fn truncate_shallow(md: String) -> String {
    if md.chars().count() <= SHALLOW_CAP_CHARS {
        return md;
    }
    let mut cut: String = md.chars().take(SHALLOW_CAP_CHARS).collect();
    cut.push_str("\n\n_（浅层记忆已达容量上限，仅保留前段；完整记忆在深层记忆库）_\n");
    cut
}

pub(crate) fn content_hash(title: &str, body: &str, group: Option<&str>) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in format!("{}\u{1}{title}\u{1}{body}", group.unwrap_or("*")).bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}")
}

pub(crate) fn age_days(ts: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| {
            (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds() as f64 / 86400.0
        })
        .unwrap_or(0.0)
}

/// 词项切分：ASCII 词（≥2）+ CJK 二元组（单字串退化为单字）
pub fn tokens(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
        if buf.chars().count() >= 2 {
            out.push(buf.clone());
        }
        buf.clear();
    }
    fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
        if buf.len() == 1 {
            out.push(buf[0].to_string());
        } else {
            for w in buf.windows(2) {
                out.push(format!("{}{}", w[0], w[1]));
            }
        }
        buf.clear();
    }

    for ch in lower.chars() {
        let is_cjk = ('\u{4e00}'..='\u{9fff}').contains(&ch);
        if is_cjk {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out.truncate(200);
    out
}
