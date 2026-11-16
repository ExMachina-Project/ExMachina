//! Model assignments belong to agents and groups; provider catalogs describe available models.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelPurpose {
    #[default]
    Chat,
    Planning,
    Coding,
    Review,
    Summary,
    Research,
    Vision,
    Speech,
    Transcribe,
    Embedding,
    ImageGen,
}

impl ModelPurpose {
    pub const ALL: [Self; 11] = [
        Self::Chat,
        Self::Planning,
        Self::Coding,
        Self::Review,
        Self::Summary,
        Self::Research,
        Self::Vision,
        Self::Speech,
        Self::Transcribe,
        Self::Embedding,
        Self::ImageGen,
    ];

    pub fn is_text(self) -> bool {
        matches!(
            self,
            Self::Chat
                | Self::Planning
                | Self::Coding
                | Self::Review
                | Self::Summary
                | Self::Research
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssignmentMode {
    #[default]
    Inherit,
    Fixed,
    Auto,
    Disabled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelAssignment {
    #[serde(default)]
    pub mode: AssignmentMode,
    #[serde(default)]
    pub model: String,
    /// Empty uses models explicitly enabled for automatic allocation in the catalog.
    #[serde(default)]
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRouting {
    #[serde(default)]
    pub default: ModelAssignment,
    #[serde(default)]
    pub purposes: BTreeMap<ModelPurpose, ModelAssignment>,
    /// Operator guidance for the commander, e.g. prefer economy models for routine work.
    #[serde(default)]
    pub instructions: String,
}

impl ModelRouting {
    pub fn set_default_model(&mut self, model: &str) {
        let model = model.trim();
        self.default = ModelAssignment {
            mode: if model.is_empty() {
                AssignmentMode::Inherit
            } else {
                AssignmentMode::Fixed
            },
            model: model.into(),
            candidates: vec![],
        };
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.instructions.chars().count() <= 4000,
            "模型分配说明最多 4000 字符"
        );
        anyhow::ensure!(
            self.default.mode != AssignmentMode::Disabled,
            "默认对话模型不能禁用"
        );
        for binding in std::iter::once(&self.default).chain(self.purposes.values()) {
            anyhow::ensure!(binding.candidates.len() <= 64, "模型候选最多 64 项");
            anyhow::ensure!(binding.model.len() <= 256, "模型引用过长");
            anyhow::ensure!(
                binding.mode != AssignmentMode::Fixed || !binding.model.trim().is_empty(),
                "固定模型不能为空"
            );
            anyhow::ensure!(
                binding.mode == AssignmentMode::Fixed || binding.model.trim().is_empty(),
                "只有固定分配可以填写模型"
            );
            anyhow::ensure!(
                binding.mode == AssignmentMode::Auto || binding.candidates.is_empty(),
                "只有自动分配可以设置候选模型"
            );
            for candidate in &binding.candidates {
                anyhow::ensure!(
                    !candidate.trim().is_empty() && candidate.len() <= 256,
                    "候选模型引用无效"
                );
            }
        }
        for (purpose, binding) in &self.purposes {
            anyhow::ensure!(
                !purpose.is_text() || binding.mode != AssignmentMode::Disabled,
                "文本能力模型不能禁用"
            );
            anyhow::ensure!(
                *purpose != ModelPurpose::Embedding || binding.mode != AssignmentMode::Auto,
                "嵌入模型需固定分配，保持同一记忆索引的向量空间一致"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelCost {
    #[default]
    Unknown,
    Economy,
    Balanced,
    Premium,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelSelection {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub purpose: ModelPurpose,
    #[serde(default)]
    pub reason: String,
}

/// Caller context follows async tool execution, including child tasks; never process-global.
#[derive(Debug, Clone)]
pub struct ModelCaller {
    pub agent_id: String,
    pub session_id: String,
}

tokio::task_local! {
    pub static MODEL_CALLER: ModelCaller;
    /// A derived executor has its own tool identity and inherits this owner's model policy.
    pub(crate) static SPAWN_CALLER: ModelCaller;
}

pub fn caller() -> Option<ModelCaller> {
    MODEL_CALLER.try_with(Clone::clone).ok()
}
