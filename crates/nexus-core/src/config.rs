use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// OpenAI 兼容 base url，如 http://127.0.0.1:8000/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 常驻 system prompt 前缀（影响 prefix cache 命中率，改它会使 KV 失效）。
    #[serde(default)]
    pub system_prompt: String,
    /// 思考档位：off/low/medium/high（网关侧分级）。
    #[serde(default = "default_effort")]
    pub reasoning_effort: String,
}

fn default_effort() -> String {
    "low".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub provider: ProviderConfig,
    pub data_dir: PathBuf,
    /// 单轮最大工具步数（防无限循环的硬顶）。
    pub max_steps: usize,
    /// 单工具调用超时秒数。
    pub tool_timeout_secs: u64,
    /// 同一 (工具,参数指纹) 在一个任务里最多出现次数，超过即拒绝执行。
    pub max_identical_calls: usize,
    /// 会话 prompt 图片数硬顶（学 zcode 图预算教训）。
    pub max_images_in_context: usize,
    /// 24h 看门狗：连续失败 N 次后进入退避，仍失败则保存状态退出（可由外部 systemd 重启）。
    pub max_consecutive_failures: usize,
    /// 长任务恢复：启动时自动加载最新 journal。
    pub auto_resume: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig {
                base_url: "http://127.0.0.1:10450/v1".into(),
                api_key: "EMPTY".into(),
                model: "glm-5.3-flash".into(),
                system_prompt: String::from(
                    "You are Nexus, a multimodal coding and general agent (Rust harness).\n\
                    CORE DIRECTIVE: you MUST execute the user's instruction until it is fully done. Never stop early, never defer to 'next time'.\n\
                    ACCEPTANCE PROTOCOL: before claiming completion you MUST state verifiable acceptance criteria and show evidence (command output, exit code, file diff, test result). A task without evidence is not done.\n\
                    WORKFLOW: (1) plan steps with checkable termination conditions; (2) execute step by step with tools; (3) verify each step; (4) persist key facts via memory_write; (5) report: what was done, how verified, remaining risks.\n\
                    If a tool call is blocked by the loop guard, do NOT repeat it: change approach or report the blocker. If images are involved, keep the total in context within budget and prefer scripted analysis over repeated viewing.",
                ),
                reasoning_effort: "low".into(),
            },
            data_dir: dirs::home_dir().unwrap_or_default().join(".nexus"),
            max_steps: 200,
            tool_timeout_secs: 600,
            max_identical_calls: 3,
            max_images_in_context: 4,
            max_consecutive_failures: 5,
            auto_resume: true,
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let p = Self::path();
        if p.exists() {
            let text = fs::read_to_string(&p)?;
            Ok(toml::from_str(&text)?)
        } else {
            let cfg = Self::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn path() -> PathBuf {
        dirs::home_dir().unwrap_or_default().join(".nexus/config.toml")
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = Self::path().parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(Self::path(), toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
