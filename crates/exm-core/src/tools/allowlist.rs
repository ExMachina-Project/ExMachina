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

