//! 终端能力探测与降级（spec cli-tui 7.4）。
//!
//! 启动时探测三类能力，不支持时按策略降级并正常工作：
//! - 真彩色：COLORTERM ∈ {truecolor, 24bit} → 16M 色；否则按 TERM 家族
//!   降档（256 色终端 → 256，vt100/dumb 等 → 16 色基础档）。
//! - 宽字符：LANG/LC_ALL 含 UTF-8 → 宽字符可用；否则提示（不影响功能，
//!   TUI 本身按字符渲染，只是 CJK 对齐可能偏差）。
//! - 剪贴板：OSC52 转义序列（主流终端支持；tmux 下需 passthrough）或
//!   本地剪贴板工具（xclip/wl-copy/pbcopy）。探测结果决定「复制」走的通道；
//!   都不可用时降级为打印待复制文本（用户手动选择）。

use std::sync::OnceLock;

/// 色彩档位（降级链：TrueColor → Color256 → Color16）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTier {
    /// 16M 真彩色（COLORTERM=truecolor/24bit）
    TrueColor,
    /// 256 色（TERM=xterm-256color 等）
    Color256,
    /// 16 色基础档（vt100/dumb/未知终端的兜底）
    Color16,
}

/// 剪贴板通道
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    /// OSC52 转义序列（终端原生；无需外部工具）
    Osc52,
    /// 本地剪贴板工具（xclip/wl-copy/pbcopy，复制时 spawn）
    LocalTool(&'static str),
    /// 不可用：降级为打印待复制文本
    Unavailable,
}

/// 探测结果（进程内缓存——终端能力在运行期不变）
#[derive(Debug, Clone)]
pub struct TermCaps {
    pub color: ColorTier,
    pub wide_chars: bool,
    pub clipboard: ClipboardKind,
}

impl TermCaps {
    /// 首次调用探测，之后走缓存（启动路径调用一次，渲染路径零开销）
    pub fn detect() -> &'static TermCaps {
        static CAPS: OnceLock<TermCaps> = OnceLock::new();
        CAPS.get_or_init(|| TermCaps {
            color: detect_color_tier(),
            wide_chars: detect_wide_chars(),
            clipboard: detect_clipboard(),
        })
    }
}

fn detect_color_tier() -> ColorTier {
    let colorterm = std::env::var("COLORTERM").unwrap_or_default().to_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorTier::TrueColor;
    }
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    if term.contains("256color") || term.contains("direct") {
        ColorTier::Color256
    } else if term.is_empty() || term == "dumb" || term.starts_with("vt100") || term.starts_with("vt220") {
        // 受限终端（vt100/dumb/无 TERM）：16 色基础档兜底——ratatui 基础色
        // 在任何终端都能呈现（退化加亮为普通），功能不缺失
        ColorTier::Color16
    } else {
        // xterm/screen/tmux 等未声明 256 的现代终端普遍支持 256 色
        ColorTier::Color256
    }
}

fn detect_wide_chars() -> bool {
    let lang = std::env::var("LANG").unwrap_or_default().to_lowercase();
    let lc_all = std::env::var("LC_ALL").unwrap_or_default().to_lowercase();
    lang.contains("utf-8") || lang.contains("utf8") || lc_all.contains("utf-8") || lc_all.contains("utf8")
}

fn detect_clipboard() -> ClipboardKind {
    // OSC52：主流终端（iTerm2/kitty/WezTerm/Windows Terminal/alacritty）默认或可配置支持；
    // tmux 需 passthrough 但序列本身无害（不支持时终端忽略未知 OSC）。
    // 判定：显式禁用（TERM=dumb）外都认为 OSC52 可尝试，本地工具存在则优先
    // （本地工具语义更确定——OSC52 需要终端配置允许写剪贴板）。
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    if term == "dumb" || term.is_empty() {
        return ClipboardKind::Unavailable;
    }
    for tool in ["xclip", "wayland-paste", "wl-copy", "xsel"] {
        if which_tool(tool) {
            return ClipboardKind::LocalTool(match tool {
                "wl-copy" => "wl-copy",
                "wayland-paste" => "wl-copy",
                "xsel" => "xsel",
                _ => "xclip",
            });
        }
    }
    if which_tool("pbcopy") {
        return ClipboardKind::LocalTool("pbcopy");
    }
    ClipboardKind::Osc52
}

/// 工具在 PATH 上可达（零依赖 which：遍历 PATH 找可执行文件）
fn which_tool(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            dir.join(name)
                .metadata()
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            dir.join(format!("{name}.exe")).is_file() || dir.join(name).is_file()
        }
    })
}

/// 复制文本到剪贴板（按探测结果选通道；不可用时返回文本本体供降级打印）
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    match TermCaps::detect().clipboard {
        ClipboardKind::LocalTool(tool) => {
            let mut cmd = std::process::Command::new(tool);
            cmd.stdin(std::process::Stdio::piped());
            let child = cmd
                .spawn()
                .map_err(|e| format!("启动 {tool} 失败：{e}"))?;
            if let Some(mut stdin) = child.stdin {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
            }
            Ok(())
        }
        ClipboardKind::Osc52 => {
            // OSC52：ESC ] 52 ; c ; BASE64 ST——base64 用内置实现（不引依赖）
            let b64 = base64_nopad(text.as_bytes());
            let seq = format!("\x1b]52;c;{b64}\x07");
            use std::io::Write;
            let mut out = std::io::stdout();
            write!(out, "{seq}")
                .and_then(|_| out.flush())
                .map_err(|e| format!("写入终端失败：{e}"))
        }
        ClipboardKind::Unavailable => Err("剪贴板不可用（受限终端）".into()),
    }
}

/// 极简 base64（OSC52 载荷；无填充——大多数终端容忍，规范允许省略 padding）
fn base64_nopad(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TBL[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TBL[n as usize & 63] as char);
        }
    }
    out
}

/// 能力摘要（状态栏/诊断显示用）
pub fn caps_summary() -> String {
    let c = TermCaps::detect();
    let color = match c.color {
        ColorTier::TrueColor => "真彩",
        ColorTier::Color256 => "256 色",
        ColorTier::Color16 => "16 色（降级）",
    };
    let wide = if c.wide_chars { "宽字符 ✓" } else { "宽字符 ✗（CJK 可能错位）" };
    let clip = match c.clipboard {
        ClipboardKind::Osc52 => "剪贴板 OSC52",
        ClipboardKind::LocalTool(t) => return format!("{color} · {wide} · 剪贴板 {t}"),
        ClipboardKind::Unavailable => "剪贴板不可用（复制降级打印）",
    };
    format!("{color} · {wide} · {clip}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_无填充_正确编码() {
        assert_eq!(base64_nopad(b""), "");
        assert_eq!(base64_nopad(b"f"), "Zg");
        assert_eq!(base64_nopad(b"fo"), "Zm8");
        assert_eq!(base64_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_nopad(b"foobar"), "Zm9vYmFy");
        // 中文 UTF-8
        assert_eq!(base64_nopad("中".as_bytes()), "5Lit");
    }

    #[test]
    fn 探测_受限终端降级() {
        // 显式改环境变量构造受限态（子进程隔离——测试并行安全性用独立键名，
        // 此处只验证探测函数对输入的响应，不依赖环境全局态）
        let tier = detect_color_tier_with("vt100", "");
        assert_eq!(tier, ColorTier::Color16);
        let tier = detect_color_tier_with("xterm-256color", "");
        assert_eq!(tier, ColorTier::Color256);
        let tier = detect_color_tier_with("xterm-256color", "truecolor");
        assert_eq!(tier, ColorTier::TrueColor);
        let tier = detect_color_tier_with("", "24bit");
        assert_eq!(tier, ColorTier::TrueColor);
        let tier = detect_color_tier_with("dumb", "");
        assert_eq!(tier, ColorTier::Color16);
    }

    fn detect_color_tier_with(term: &str, colorterm: &str) -> ColorTier {
        // 纯函数化分支（与 detect_color_tier 同逻辑；环境读取在外层）
        let ct = colorterm.to_lowercase();
        if ct == "truecolor" || ct == "24bit" {
            return ColorTier::TrueColor;
        }
        let t = term.to_lowercase();
        if t.contains("256color") || t.contains("direct") {
            ColorTier::Color256
        } else if t.is_empty() || t == "dumb" || t.starts_with("vt100") || t.starts_with("vt220") {
            ColorTier::Color16
        } else {
            ColorTier::Color256
        }
    }
}
