//! tools/allowlist.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

/// 终端命令白名单规则：命令名 → 允许的子命令前缀（空 = 该命令整体放行）。
/// 只匹配**首命令**且拒绝 shell 连接符（`&&` `|` `;` 等），避免「白名单命令 + 追加危险命令」绕过。
struct AllowRule {
    cmd: &'static str,
    subs: &'static [&'static str],
}

const TERMINAL_ALLOW: &[AllowRule] = &[
    AllowRule {
        cmd: "dir",
        subs: &[],
    },
    AllowRule {
        cmd: "ls",
        subs: &[],
    },
    AllowRule {
        cmd: "type",
        subs: &[],
    },
    AllowRule {
        cmd: "echo",
        subs: &[],
    },
    AllowRule {
        cmd: "findstr",
        subs: &[],
    },
    AllowRule {
        cmd: "where",
        subs: &[],
    },
    AllowRule {
        cmd: "which",
        subs: &[],
    },
    AllowRule {
        cmd: "whoami",
        subs: &[],
    },
    AllowRule {
        cmd: "uname",
        subs: &[],
    },
    AllowRule {
        cmd: "pwd",
        subs: &[],
    },
    AllowRule {
        cmd: "date",
        subs: &[],
    },
    AllowRule {
        cmd: "head",
        subs: &[],
    },
    AllowRule {
        cmd: "tail",
        subs: &[],
    },
    AllowRule {
        cmd: "cat",
        subs: &[],
    },
    AllowRule {
        cmd: "wc",
        subs: &[],
    },
    AllowRule {
        cmd: "diff",
        subs: &[],
    },
    AllowRule {
        cmd: "mkdir",
        subs: &[],
    },
    AllowRule {
        cmd: "cp",
        subs: &[],
    },
    AllowRule {
        cmd: "mv",
        subs: &[],
    },
    AllowRule {
        cmd: "touch",
        subs: &[],
    },
    AllowRule {
        cmd: "rg",
        subs: &[],
    },
    AllowRule {
        cmd: "grep",
        subs: &[],
    },
    AllowRule {
        cmd: "find",
        subs: &[],
    },
    AllowRule {
        cmd: "sed",
        subs: &["-n"],
    },
    AllowRule {
        cmd: "node",
        subs: &[],
    },
    AllowRule {
        cmd: "npx",
        subs: &[],
    },
    AllowRule {
        cmd: "tsc",
        subs: &[],
    },
    AllowRule {
        cmd: "deno",
        subs: &["test", "run", "check", "fmt", "lint", "info", "cache"],
    },
    AllowRule {
        cmd: "bun",
        subs: &["test", "run", "install", "x", "build"],
    },
    AllowRule {
        cmd: "pytest",
        subs: &[],
    },
    AllowRule {
        cmd: "jest",
        subs: &[],
    },
    AllowRule {
        cmd: "vitest",
        subs: &[],
    },
    AllowRule {
        cmd: "npm",
        subs: &[
            "run", "test", "ci", "install", "ls", "view", "why", "audit", "outdated", "pack",
        ],
    },
    AllowRule {
        cmd: "pnpm",
        subs: &[
            "run", "test", "install", "list", "ls", "why", "audit", "outdated",
        ],
    },
    AllowRule {
        cmd: "yarn",
        subs: &["run", "test", "install", "list", "why", "audit", "outdated"],
    },
    AllowRule {
        cmd: "git",
        subs: &[
            "status",
            "log",
            "diff",
            "show",
            "branch",
            "fetch",
            "add",
            "commit",
            "checkout",
            "switch",
            "restore",
            "stash",
            "rev-parse",
            "ls-files",
            "blame",
            "describe",
            "tag",
            "remote",
            "config",
            "apply",
            "merge-base",
            "shortlog",
            "grep",
            "worktree",
            "init",
        ],
    },
    AllowRule {
        cmd: "cargo",
        subs: &[
            "build",
            "check",
            "test",
            "run",
            "bench",
            "fmt",
            "clippy",
            "doc",
            "metadata",
            "tree",
            "nextest",
            "update",
            "add",
            "remove",
            "fetch",
            "verify-project",
            "locate-project",
            "pkgid",
            "search",
            "info",
            "clean",
        ],
    },
    AllowRule {
        cmd: "rustc",
        subs: &[],
    },
    AllowRule {
        cmd: "rustfmt",
        subs: &[],
    },
    AllowRule {
        cmd: "python",
        subs: &[],
    },
    AllowRule {
        cmd: "python3",
        subs: &[],
    },
    AllowRule {
        cmd: "pip",
        subs: &["list", "show", "freeze", "download"],
    },
    AllowRule {
        cmd: "go",
        subs: &[
            "build", "test", "run", "vet", "fmt", "mod", "get", "generate", "doc", "env", "list",
        ],
    },
    AllowRule {
        cmd: "gcc",
        subs: &[],
    },
    AllowRule {
        cmd: "g++",
        subs: &[],
    },
    AllowRule {
        cmd: "clang",
        subs: &[],
    },
    AllowRule {
        cmd: "make",
        subs: &[],
    },
    AllowRule {
        cmd: "cmake",
        subs: &[],
    },
    AllowRule {
        cmd: "java",
        subs: &[],
    },
    AllowRule {
        cmd: "javac",
        subs: &[],
    },
    AllowRule {
        cmd: "mvn",
        subs: &[],
    },
    AllowRule {
        cmd: "gradle",
        subs: &[],
    },
    AllowRule {
        cmd: "dotnet",
        subs: &["build", "test", "run", "restore", "format", "publish"],
    },
    AllowRule {
        cmd: "php",
        subs: &[],
    },
    AllowRule {
        cmd: "composer",
        subs: &["install", "update", "test", "dump-autoload"],
    },
    AllowRule {
        cmd: "ruby",
        subs: &[],
    },
    AllowRule {
        cmd: "gem",
        subs: &["list", "install", "info"],
    },
    AllowRule {
        cmd: "rake",
        subs: &[],
    },
    // ---- harness 第二轮扩充（spec: agent-terminal-tools）----
    // 信息只读类
    AllowRule {
        cmd: "stat",
        subs: &[],
    },
    AllowRule {
        cmd: "du",
        subs: &[],
    },
    AllowRule {
        cmd: "df",
        subs: &[],
    },
    AllowRule {
        cmd: "env",
        subs: &[],
    },
    AllowRule {
        cmd: "ps",
        subs: &[],
    },
    AllowRule {
        cmd: "file",
        subs: &[],
    },
    AllowRule {
        cmd: "basename",
        subs: &[],
    },
    AllowRule {
        cmd: "dirname",
        subs: &[],
    },
    AllowRule {
        cmd: "realpath",
        subs: &[],
    },
    AllowRule {
        cmd: "md5sum",
        subs: &[],
    },
    AllowRule {
        cmd: "sha256sum",
        subs: &[],
    },
    AllowRule {
        cmd: "free",
        subs: &[],
    },
    AllowRule {
        cmd: "uptime",
        subs: &[],
    },
    AllowRule {
        cmd: "hostname",
        subs: &[],
    },
    AllowRule {
        cmd: "id",
        subs: &[],
    },
    AllowRule {
        cmd: "groups",
        subs: &[],
    },
    AllowRule {
        cmd: "locale",
        subs: &[],
    },
    // 文本处理类
    AllowRule {
        cmd: "awk",
        subs: &[],
    },
    AllowRule {
        cmd: "sort",
        subs: &[],
    },
    AllowRule {
        cmd: "uniq",
        subs: &[],
    },
    AllowRule {
        cmd: "xargs",
        subs: &[],
    },
    AllowRule {
        cmd: "cut",
        subs: &[],
    },
    AllowRule {
        cmd: "tr",
        subs: &[],
    },
    AllowRule {
        cmd: "column",
        subs: &[],
    },
    AllowRule {
        cmd: "bc",
        subs: &[],
    },
    AllowRule {
        cmd: "jq",
        subs: &[],
    },
    AllowRule {
        cmd: "yq",
        subs: &[],
    },
    // 网络获取类（联网语义仍受沙箱联网闸门后置拦截）
    AllowRule {
        cmd: "curl",
        subs: &[],
    },
    AllowRule {
        cmd: "wget",
        subs: &[],
    },
    AllowRule {
        cmd: "ping",
        subs: &[],
    },
    AllowRule {
        cmd: "dig",
        subs: &[],
    },
    AllowRule {
        cmd: "nslookup",
        subs: &[],
    },
    AllowRule {
        cmd: "host",
        subs: &[],
    },
    AllowRule {
        cmd: "ip",
        subs: &["addr", "route", "link", "a", "r", "l", "-brief"],
    },
    // 压缩归档类
    AllowRule {
        cmd: "tar",
        subs: &[],
    },
    AllowRule {
        cmd: "unzip",
        subs: &[],
    },
    AllowRule {
        cmd: "zip",
        subs: &[],
    },
    AllowRule {
        cmd: "gzip",
        subs: &[],
    },
    AllowRule {
        cmd: "gunzip",
        subs: &[],
    },
    AllowRule {
        cmd: "xz",
        subs: &[],
    },
    AllowRule {
        cmd: "zcat",
        subs: &[],
    },
    // 权限与时间类
    AllowRule {
        cmd: "chmod",
        subs: &[],
    },
    AllowRule {
        cmd: "chown",
        subs: &[],
    },
    AllowRule {
        cmd: "sleep",
        subs: &[],
    },
    // 远程同步类（联网语义仍受沙箱联网闸门后置拦截）
    AllowRule {
        cmd: "ssh",
        subs: &[],
    },
    AllowRule {
        cmd: "scp",
        subs: &[],
    },
    AllowRule {
        cmd: "rsync",
        subs: &[],
    },
    AllowRule {
        cmd: "sftp",
        subs: &[],
    },
    // 容器类（只读 + exec/run；高危子命令仍走审批）
    AllowRule {
        cmd: "docker",
        subs: &["ps", "images", "inspect", "logs", "exec", "run", "version"],
    },
    // 数据库类（本地文件库；psql/mysql 走审批）
    AllowRule {
        cmd: "sqlite3",
        subs: &[],
    },
    // 媒体与工具类
    AllowRule {
        cmd: "ffmpeg",
        subs: &[],
    },
    AllowRule {
        cmd: "convert",
        subs: &[],
    },
    AllowRule {
        cmd: "openssl",
        subs: &[],
    },
    AllowRule {
        cmd: "base64",
        subs: &[],
    },
    AllowRule {
        cmd: "uuidgen",
        subs: &[],
    },
    AllowRule {
        cmd: "xxd",
        subs: &[],
    },
];

/// shell 连接符清单：分两类——
/// · 链式分隔（| && || ;）：可安全放行，条件是**每一段**首命令都过白名单
///   （危险命令藏在任何一段都会被拒，攻击面与单命令一致）
/// · 逃逸/写入语义（` $( > < &）：白名单管不住（命令替换绕过解析、
///   重定向写文件、后台执行），一律拒绝
const SHELL_CHAIN_SEPARATORS: &[&str] = &["&&", "||", "|", ";"];
const SHELL_ESCAPE_MARKS: &[&str] = &["`", "$(", ">", "<", "&"];

/// 剥掉引号包裹的片段（引号内的 `>` `|` 等是普通字符，不应误判为连接符）
pub(crate) fn strip_quoted(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                } else {
                    out.push(ch);
                }
            }
        }
    }
    out
}

pub(crate) fn contains_shell_chain(cmd: &str) -> bool {
    let bare = strip_quoted(cmd);
    SHELL_ESCAPE_MARKS.iter().any(|m| bare.contains(m))
}

/// 链式命令分段（引号感知）：`a | b && c` → ["a", "b", "c"]。
/// 空段（如 `a |` 收尾）不产出——由调用方按「非白名单段」拒绝。
pub(crate) fn split_chain_segments(cmd: &str) -> Vec<String> {
    let mut segs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if let Some(q) = quote {
            cur.push(ch);
            if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '|' if i + 1 < chars.len() && chars[i + 1] == '|' => {
                if !cur.trim().is_empty() {
                    segs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                i += 1; // 消耗第二个 |
            }
            '|' | '&' if i + 1 < chars.len() && chars[i + 1] == '&' => {
                if !cur.trim().is_empty() {
                    segs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                i += 1; // 消耗第二个 &
            }
            '|' | ';' => {
                if !cur.trim().is_empty() {
                    segs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
            _ => cur.push(ch),
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        segs.push(cur);
    }
    segs
}

/// 链式命令整体放行判定：每一段首命令（+子命令前缀）都过白名单才放行。
/// 与单命令同攻击面——危险命令藏在任何一段都拒绝。
pub(crate) fn chain_allowed(cmd: &str) -> bool {
    // 链式分隔符（| && || ;）存在：逐段过白名单（每段首命令都过才放行，
    // 危险命令藏在任何一段都拒绝——攻击面与单命令一致）
    let bare = strip_quoted(cmd);
    let has_chain_sep = SHELL_CHAIN_SEPARATORS.iter().any(|m| bare.contains(m));
    if has_chain_sep {
        return split_chain_segments(cmd)
            .iter()
            .all(|seg| terminal_allowed(seg));
    }
    terminal_allowed(cmd)
}

/// 白名单判定：首命令 + （有子命令约束时）子命令前缀
pub(crate) fn terminal_allowed(cmd: &str) -> bool {
    let lowered = cmd.trim().to_lowercase();
    let mut parts = lowered.split_whitespace();
    let Some(head_raw) = parts.next() else {
        return false;
    };
    let head = head_raw.trim_end_matches(".exe");
    let Some(rule) = TERMINAL_ALLOW.iter().find(|r| r.cmd == head) else {
        return false;
    };
    if rule.subs.is_empty() {
        return true;
    }
    let rest: Vec<&str> = parts.collect();
    rule.subs.iter().any(|s| {
        let want: Vec<&str> = s.split_whitespace().collect();
        rest.len() >= want.len() && want.iter().zip(rest.iter()).all(|(a, b)| a == b)
    })
}

/// 高危命令特征（审批闸门 risky 模式匹配子串）
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "rm ",
    "rm -",
    "del ",
    "rmdir",
    "rd /s",
    "format ",
    "shutdown",
    "taskkill",
    "reg delete",
    "reg add",
    "dd if=",
    "mkfs",
    "git push --force",
    "git push -f",
    "git reset --hard",
    "git clean",
    "git rebase",
    "drop table",
    "drop database",
    "truncate table",
    "remove-item",
    "invoke-expression",
    "curl | sh",
    "curl | bash",
];

pub fn is_destructive_command(lowered: &str) -> bool {
    DESTRUCTIVE_PATTERNS.iter().any(|p| lowered.contains(p))
}

pub(crate) fn shell_unsafe_char(c: char) -> bool {
    matches!(
        c,
        '"' | '\''
            | '`'
            | '$'
            | '&'
            | '|'
            | ';'
            | '<'
            | '>'
            | '('
            | ')'
            | '%'
            | '!'
            | '^'
            | '\\'
            | '\n'
            | '\r'
            | '\t'
    )
}

/// 实参包裹：Windows 双引号 / POSIX 单引号（调用方已先行做字符白名单校验）
pub(crate) fn shell_quote(v: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("\"{v}\"")
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("'{v}'")
    }
}

// ---------------------------------------------------------------- 记忆工具与工具面快照（组 8 单测）


// ---------------------------------------------------------------- root 提权识别（agent-exec-confinement）

/// 提权/切换用户命令首 token（agent-exec-confinement）：
/// sudo/doas/run0 是提权（root 或配置的目标用户）；su/runuser 是切换用户语义；
/// sudo -u <user> 是定向切换。首 token 命中即整段按提权处理——白名单匹配的是
/// 去掉提权前缀后的命令内容,提权前缀本身越权,必须走人工审批。
pub(crate) const ELEVATION_HEADS: &[&str] = &["sudo", "doas", "run0", "su", "runuser"];

/// 单段命令是否提权（首 token 判定；参数含 -u 切换也算,由首 token 覆盖——
/// sudo/runuser 本身就是切换语义,无需再看参数）
pub(crate) fn is_elevation_segment(seg: &str) -> bool {
    let binding = seg.trim().to_lowercase();
    let head = binding.split_whitespace().next();
    head.is_some_and(|h| ELEVATION_HEADS.contains(&h))
}

/// 整条命令是否含提权段：链式命令任一段提权即整条按提权处理
/// （`apt list && sudo apt install` 的第二段越权,不能因第一段白名单放行）
pub(crate) fn is_elevation_command(cmd: &str) -> bool {
    let bare = strip_quoted(cmd);
    let has_chain_sep = SHELL_CHAIN_SEPARATORS.iter().any(|m| bare.contains(m));
    if has_chain_sep {
        return split_chain_segments(cmd).iter().any(|seg| is_elevation_segment(seg));
    }
    is_elevation_segment(cmd)
}

/// 提权代执行前处理：剥离冗余提权前缀（root 执行时 sudo/doas/run0 是多余壳），
/// 保留切换语义命令（su/sudo -u/runuser 的目标用户是命令语义的一部分）。
pub(crate) fn strip_redundant_elevation(cmd: &str) -> String {
    let trimmed = cmd.trim();
    let lowered = trimmed.to_lowercase();
    let Some(first) = trimmed.split_whitespace().next() else {
        return trimmed.to_string();
    };
    match lowered.split_whitespace().next().unwrap_or("") {
        // 纯提权壳:root 下冗余,剥掉首 token 保留余部
        "sudo" | "doas" | "run0" => {
            // sudo -u <user> 是定向切换:保留原样(切换语义)
            let rest: Vec<&str> = trimmed.split_whitespace().skip(1).collect();
            if rest.first() == Some(&"-u") {
                trimmed.to_string()
            } else {
                rest.join(" ")
            }
        }
        // su/runuser:切换用户是命令语义,原样保留
        _ => first.to_string() + &trimmed[first.len()..],
    }
}

// ---------------------------------------------------------------- 会话授权前缀（approval-ergonomics）

/// 会话授权的命令前缀口径：命令名 + 首子命令（剥提权前缀）。
/// 与白名单「命令 + 子命令」逐段匹配口径对齐——grant 的放行面不宽于
/// 用户批准时所见的命令类别（`cargo build --release` → `cargo build`）。
/// 破坏性/提权判定不在此处：grant 表写入侧（approval_decide）负责红线校验。
pub fn command_head(cmd: &str) -> String {
    let trimmed = cmd.trim();
    // 剥提权壳（sudo/doas/run0 非定向切换；su/runuser 是切换语义,不剥——
    // 但这类命令按红线根本进不了 grant 表,此处仅保守处理）
    let stripped = strip_redundant_elevation(trimmed);
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    match tokens.len() {
        0 => String::new(),
        1 => tokens[0].to_lowercase(),
        _ => format!("{} {}", tokens[0], tokens[1]).to_lowercase(),
    }
}

#[cfg(test)]
mod elevation_tests {
    use super::*;

    #[test]
    fn 识别各提权前缀() {
        assert!(is_elevation_command("sudo apt install -y tmux"));
        assert!(is_elevation_command("doas apk add curl"));
        assert!(is_elevation_command("run0 systemctl restart nginx"));
        assert!(is_elevation_command("su - postgres -c 'psql -c \"select 1\"'"));
        assert!(is_elevation_command("runuser -u www -- nginx -t"));
        assert!(is_elevation_command("sudo -u deploy git pull"));
    }

    #[test]
    fn 普通命令不误报() {
        assert!(!is_elevation_command("apt install -y tmux"));
        assert!(!is_elevation_command("git status"));
        assert!(!is_elevation_command("sudoers edit /etc/sudoers"));
        // "su" 出现在参数位置不是提权
        assert!(!is_elevation_command("echo su"));
        assert!(!is_elevation_command("cat /etc/sudoers"));
    }

    #[test]
    fn 链式命令任一段提权即整条提权() {
        assert!(is_elevation_command("apt list && sudo apt install -y tmux"));
        assert!(is_elevation_command("git pull; su - root"));
        assert!(!is_elevation_command("git pull && git status"));
    }

    #[test]
    fn 引号内分隔符不误判链式() {
        // 引号内的 | 不是链分隔符
        assert!(is_elevation_command("sudo sh -c 'echo a | b'"));
        assert!(!is_elevation_command("echo 'sudo apt install'"));
    }

    #[test]
    fn 剥离冗余提权壳() {
        assert_eq!(strip_redundant_elevation("sudo apt install -y tmux"), "apt install -y tmux");
        assert_eq!(strip_redundant_elevation("doas apk add curl"), "apk add curl");
        assert_eq!(strip_redundant_elevation("run0 systemctl restart nginx"), "systemctl restart nginx");
    }

    #[test]
    fn 切换语义保留原样() {
        assert_eq!(
            strip_redundant_elevation("su - postgres -c 'psql -c \"select 1\"'"),
            "su - postgres -c 'psql -c \"select 1\"'"
        );
        assert_eq!(strip_redundant_elevation("sudo -u deploy git pull"), "sudo -u deploy git pull");
        assert_eq!(strip_redundant_elevation("runuser -u www -- nginx -t"), "runuser -u www -- nginx -t");
    }

    #[test]
    fn 大小写不敏感() {
        assert!(is_elevation_command("SUDO apt install"));
        assert!(is_elevation_command("Sudo apt install"));
        assert_eq!(strip_redundant_elevation("SUDO id"), "id");
    }
}

#[cfg(test)]
mod command_head_tests {
    use super::*;

    #[test]
    fn 普通命令取名加首子命令() {
        assert_eq!(command_head("cargo build --release"), "cargo build");
        assert_eq!(command_head("mkdir -p /tmp/x"), "mkdir -p");
        assert_eq!(command_head("git status"), "git status");
    }

    #[test]
    fn 单命令只取名() {
        assert_eq!(command_head("ls"), "ls");
        assert_eq!(command_head("pytest"), "pytest");
    }

    #[test]
    fn 提权壳剥除后取头() {
        assert_eq!(command_head("sudo apt install -y tmux"), "apt install");
        assert_eq!(command_head("doas apk add curl"), "apk add");
    }

    #[test]
    fn 切换语义命令保留原头() {
        // su/runuser/sudo -u 是切换语义,不剥首 token(这类命令按红线进不了 grant,
        // 口径上仍保持「用户所见即所批」)
        assert_eq!(command_head("su - postgres -c 'psql'"), "su -");
        assert_eq!(command_head("sudo -u deploy git pull"), "sudo -u");
    }

    #[test]
    fn 空白与空命令容错() {
        assert_eq!(command_head("   "), "");
        assert_eq!(command_head(""), "");
        assert_eq!(command_head("  cargo   build  "), "cargo build");
    }

    #[test]
    fn 大小写归一() {
        assert_eq!(command_head("Cargo BUILD --release"), "cargo build");
    }
}
