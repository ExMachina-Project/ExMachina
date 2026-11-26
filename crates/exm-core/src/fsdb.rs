//! Filesystem-backed JSON/JSONL store used by the current Core `Store` implementation.
//! `SqliteStore` remains a separate implementation; Core does not select it.
//!
//! 设计：
//!   - 单条文档：`{root}/{collection}/{id}.json`，原子写（临时文件 + rename）
//!   - 追加日志：`{root}/{collection}/{key}.jsonl`（事件、消息、审计），读时解析
//!   - 分桶索引：`{root}/{collection}/{bucket}.json`（倒排词项等大映射），按哈希分桶，
//!     写入只重写受影响的分桶 → 避免整表重写

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const TERM_BUCKETS: usize = 64;

pub struct FsDb {
    root: PathBuf,
    /// 保护分桶文件的读改写（单进程内串行化即可）
    lock: Mutex<()>,
}

impl FsDb {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        Ok(FsDb {
            root,
            lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn coll_dir(&self, collection: &str) -> Result<PathBuf> {
        let dir = self.root.join(collection);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn doc_path(&self, collection: &str, id: &str) -> Result<PathBuf> {
        Ok(self
            .coll_dir(collection)?
            .join(format!("{}.json", sanitize(id))))
    }

    // ---------------- 文档读写 ----------------

    pub fn put<T: Serialize>(&self, collection: &str, id: &str, value: &T) -> Result<()> {
        let path = self.doc_path(collection, id)?;
        let data = serde_json::to_vec_pretty(value)?;
        atomic_write(&path, &data).with_context(|| format!("写入失败: {}", path.display()))
    }

    pub fn get<T: DeserializeOwned>(&self, collection: &str, id: &str) -> Result<Option<T>> {
        let path = self.doc_path(collection, id)?;
        match std::fs::read(&path) {
            Ok(bytes) => {
                Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                    format!("解析失败: {}", path.display())
                })?))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete(&self, collection: &str, id: &str) -> Result<()> {
        let path = self.doc_path(collection, id)?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn exists(&self, collection: &str, id: &str) -> Result<bool> {
        Ok(self.doc_path(collection, id)?.exists())
    }

    /// 列出集合内全部文档（按 id 排序，保证确定性）
    pub fn list<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>> {
        let dir = self.coll_dir(collection)?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        files.sort();
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            let bytes = std::fs::read(&f)?;
            match serde_json::from_slice::<T>(&bytes) {
                Ok(v) => out.push(v),
                Err(e) => eprintln!("[fsdb] 跳过损坏文档 {}: {e}", f.display()),
            }
        }
        Ok(out)
    }

    pub fn count(&self, collection: &str) -> Result<usize> {
        let dir = self.coll_dir(collection)?;
        Ok(std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .count())
    }

    // ---------------- 追加日志（JSONL） ----------------

    pub fn append_line<T: Serialize>(&self, collection: &str, key: &str, value: &T) -> Result<()> {
        use std::io::Write;
        let path = self
            .coll_dir(collection)?
            .join(format!("{}.jsonl", sanitize(key)));
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(&line)?;
        Ok(())
    }

    pub fn delete_lines(&self, collection: &str, key: &str) -> Result<()> {
        let path = self
            .coll_dir(collection)?
            .join(format!("{}.jsonl", sanitize(key)));
        remove_if_exists(&path.with_extension("jsonl.tmp"))?;
        Ok(remove_if_exists(&path)?)
    }

    /// 读取追加日志；`tail` 为 0 表示全部
    pub fn read_lines<T: DeserializeOwned>(
        &self,
        collection: &str,
        key: &str,
        tail: usize,
    ) -> Result<Vec<T>> {
        let path = self
            .coll_dir(collection)?
            .join(format!("{}.jsonl", sanitize(key)));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut items = parse_lines(&content);
        if tail > 0 && items.len() > tail {
            items = items.split_off(items.len() - tail);
        }
        Ok(items)
    }

    /// 在解码前按实际字节限制日志读取，不能信任调用方保存的大小。
    pub(crate) fn read_lines_bounded<T: DeserializeOwned>(
        &self,
        collection: &str,
        key: &str,
        limit: usize,
    ) -> Result<Vec<T>> {
        use std::io::Read;

        let path = self
            .root
            .join(collection)
            .join(format!("{}.jsonl", sanitize(key)));
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        let file = match open_bounded_log(options, &path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= limit, "日志超过读取上限（{limit} 字节）");
        // 只容忍末尾未写完的一行；中间损坏不可伪装成连续的输出。
        let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        std::str::from_utf8(&bytes[..complete])?
            .lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }

    /// 重写追加日志（用于容量裁剪）
    pub fn rewrite_lines<T: Serialize>(
        &self,
        collection: &str,
        key: &str,
        items: &[T],
    ) -> Result<()> {
        let path = self
            .coll_dir(collection)?
            .join(format!("{}.jsonl", sanitize(key)));
        let mut buf = Vec::new();
        for it in items {
            serde_json::to_writer(&mut buf, it)?;
            buf.push(b'\n');
        }
        atomic_write(&path, &buf)
    }

    /// 单写者日志：有空间就追加，否则把有界尾部写入固定暂存文件后替换。
    pub(crate) fn write_lines_bounded<'a, T: Serialize + 'a>(
        &self,
        collection: &str,
        key: &str,
        items: impl IntoIterator<Item = &'a T>,
        append_at: Option<usize>,
        limit: usize,
    ) -> Result<()> {
        use std::io::Write;

        let path = self
            .coll_dir(collection)?
            .join(format!("{}.jsonl", sanitize(key)));
        let capacity = limit.saturating_sub(append_at.unwrap_or(0));
        let mut buf = Vec::new();
        for item in items {
            serde_json::to_writer(&mut buf, item)?;
            buf.push(b'\n');
            anyhow::ensure!(buf.len() <= capacity, "后台日志写入超过字节上限");
        }
        if let Some(offset) = append_at {
            let mut options = std::fs::OpenOptions::new();
            options.create(offset == 0).append(true);
            let mut file = open_bounded_log(options, &path)?;
            anyhow::ensure!(
                file.metadata()?.len() == offset as u64,
                "后台日志未处于预期追加位置"
            );
            return Ok(file.write_all(&buf)?);
        }
        let scratch = path.with_extension("jsonl.tmp");
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&scratch)?;
        let result = file.write_all(&buf);
        drop(file);
        let result = result.and_then(|()| replace_file(&scratch, &path));
        if result.is_err() {
            let _ = remove_if_exists(&scratch);
        }
        result.map_err(Into::into)
    }

    /// 枚举集合内全部追加日志的行（跨 key 合并；坏行跳过不致命）。
    /// 与 `list`（.json 文档集合）相对——追加日志集合（tool_audit 等）的
    /// 全量读取用这里。
    pub fn list_all_lines<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>> {
        let dir = self.coll_dir(collection)?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .collect();
        files.sort();
        let mut out = Vec::new();
        for f in files {
            let content = match std::fs::read_to_string(&f) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<T>(line) {
                    Ok(v) => out.push(v),
                    Err(e) => eprintln!("[fsdb] 跳过损坏日志行 {}: {e}", f.display()),
                }
            }
        }
        Ok(out)
    }

    // ---------------- 分桶大映射（倒排索引等） ----------------

    pub fn bucket_id(term: &str) -> usize {
        let mut h: u64 = 1469598103934665603;
        for b in term.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        (h as usize) % TERM_BUCKETS
    }

    /// 读-改-写一个分桶映射（回调内完成修改）。
    /// `key` 同时用于定位分桶与作为映射键传给回调，避免借用冲突。
    pub fn with_bucket_map<V, F>(&self, collection: &str, key: &str, mut f: F) -> Result<()>
    where
        V: DeserializeOwned + Serialize + Clone,
        F: FnMut(&str, &mut std::collections::HashMap<String, V>),
    {
        let _guard = self.lock.lock();
        let dir = self.coll_dir(collection)?;
        let path = dir.join(format!("bucket-{:02}.json", Self::bucket_id(key)));
        let mut map: std::collections::HashMap<String, V> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => std::collections::HashMap::new(),
        };
        f(key, &mut map);
        atomic_write(&path, &serde_json::to_vec(&map)?)
    }

    /// 读取包含指定键的分桶映射
    pub fn read_bucket_map<V: DeserializeOwned>(
        &self,
        collection: &str,
        term: &str,
    ) -> Result<std::collections::HashMap<String, V>> {
        let dir = self.coll_dir(collection)?;
        let path = dir.join(format!("bucket-{:02}.json", Self::bucket_id(term)));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(_) => Ok(std::collections::HashMap::new()),
        }
    }

    /// 清空分桶目录（索引重建用）
    pub fn clear_buckets(&self, collection: &str) -> Result<()> {
        let dir = self.coll_dir(collection)?;
        for e in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
            if e.path()
                .file_name()
                .map(|n| n.to_string_lossy().starts_with("bucket-"))
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
        Ok(())
    }

    /// 分桶映射中的键总数（倒排词项数统计）
    pub fn bucket_key_count(&self, collection: &str) -> Result<usize> {
        let dir = self.coll_dir(collection)?;
        let mut total = 0;
        for e in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
            let name = e
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !name.starts_with("bucket-") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(e.path()) {
                let map: std::collections::HashMap<String, serde_json::Value> =
                    serde_json::from_slice(&bytes).unwrap_or_default();
                total += map.len();
            }
        }
        Ok(total)
    }
}

fn parse_lines<T: DeserializeOwned>(content: &str) -> Vec<T> {
    // 容忍崩溃残留的半行；需要连续序号的读者另行验证日志范围。
    content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// 原子写：同目录唯一临时文件 + rename。
/// tmp 带唯一后缀（`{name}.{new_id()}.tmp`）：并发写同目标不共用同一 tmp 文件，
/// 避免交错后 rename 出半截内容（审计竞态 P1-5 场景B）。
/// rename 语义按平台分流：Linux/POSIX rename 原子覆盖目标——不存在 remove→rename
/// 之间的"文件不存在"窗口（场景A：读者瞬时得 NotFound，flow 绑定被顺手删）；
/// Windows rename 到已存在目标需先删（std 保持 errno 行为），保留回退。
fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", crate::types::new_id()));
    let result = std::fs::write(&tmp, data).and_then(|()| replace_file(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(Into::into)
}

fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(not(unix))]
    remove_if_exists(to)?;
    std::fs::rename(from, to)
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_bounded_log(options: std::fs::OpenOptions, path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = options;
        // 不跟随末端符号链接，也不因意外的 FIFO 阻塞读写。
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        options
    };
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(std::io::Error::other("后台日志不是普通文件"));
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("后台日志不是普通文件"));
    }
    Ok(file)
}

/// 文件名安全化：路径分隔符与保留字符替换
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r' => '_',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 文档读写与追加日志() {
        let dir = std::env::temp_dir().join(format!("fsdb-{}", uuid::Uuid::new_v4()));
        let db = FsDb::open(&dir).unwrap();
        db.put("sessions", "s1", &serde_json::json!({"id":"s1","n":1}))
            .unwrap();
        let got: Option<serde_json::Value> = db.get("sessions", "s1").unwrap();
        assert_eq!(got.unwrap()["n"], 1);
        assert_eq!(db.list::<serde_json::Value>("sessions").unwrap().len(), 1);

        for i in 0..5 {
            db.append_line("events", "s1", &serde_json::json!({"i": i}))
                .unwrap();
        }
        let tail: Vec<serde_json::Value> = db.read_lines("events", "s1", 2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1]["i"], 4);

        db.with_bucket_map::<Vec<String>, _>("terms", "记忆", |key, m| {
            m.entry(key.to_string()).or_default().push("e1".into());
        })
        .unwrap();
        let m: std::collections::HashMap<String, Vec<String>> =
            db.read_bucket_map("terms", "记忆").unwrap();
        assert_eq!(m["记忆"], vec!["e1".to_string()]);

        db.delete("sessions", "s1").unwrap();
        assert!(db
            .get::<serde_json::Value>("sessions", "s1")
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
