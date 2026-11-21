//! 按观测顺序保存 stdout/stderr 的有界尾部，游标不随日志裁剪重置。

use super::managed_process::Stream;
use crate::store::{Store, TerminalJobOutputChunk, TerminalJobRecord};
use anyhow::{ensure, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::VecDeque;

const MAX_PAGE_BYTES: u64 = 8192;
const MAX_PAGE_CHUNKS: usize = 64;
const MAX_WAIT_MS: u64 = 2000;
const PREVIEW_CHARS: usize = 200;

#[derive(Default)]
pub(super) struct OutputTail {
    chunks: VecDeque<(usize, TerminalJobOutputChunk)>,
    utf8_pending: [Vec<u8>; 2],
}

impl OutputTail {
    pub fn decode(&mut self, stream: Stream, bytes_input: &[u8], eof: bool) -> String {
        let pending = &mut self.utf8_pending[match stream {
            Stream::Stdout => 0,
            Stream::Stderr => 1,
        }];
        let mut bytes = std::mem::take(pending);
        bytes.extend_from_slice(bytes_input);
        if !eof {
            // 管道 read 可以从一个 UTF-8 字符中间切开。只暂存可能未读完的最后一个字符。
            for from in (bytes.len().saturating_sub(3)..bytes.len()).rev() {
                if std::str::from_utf8(&bytes[from..])
                    .is_err_and(|error| error.valid_up_to() == 0 && error.error_len().is_none())
                {
                    pending.extend_from_slice(&bytes[from..]);
                    bytes.truncate(from);
                    break;
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn append(
        &mut self,
        store: &Store,
        record: &mut TerminalJobRecord,
        stream: Stream,
        text: &str,
        limit: usize,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let mut end = record.output_end_byte.unwrap_or(0);
        let mut sequence = record.output_chunks;
        let mut incoming = VecDeque::new();
        let mut incoming_bytes = 0;
        let mut rest = text;
        // 一个 ASCII 控制字符最多转义为六字节；为 JSON 封套保留 128 字节。
        let chunk_bytes = ((limit - 128) / 6).min(MAX_PAGE_BYTES as usize);
        while !rest.is_empty() {
            let part = utf8_prefix(rest, chunk_bytes);
            let chunk = TerminalJobOutputChunk {
                sequence,
                stream: match stream {
                    Stream::Stdout => "stdout",
                    Stream::Stderr => "stderr",
                }
                .into(),
                text: part.into(),
                start_byte: Some(end),
            };
            let size = encoded_len(&chunk)?;
            incoming_bytes += size;
            incoming.push_back((size, chunk));
            end += part.len() as u64;
            sequence += 1;
            rest = &rest[part.len()..];
        }

        let rotate = record.log_bytes + incoming_bytes > limit;
        let mut evict = 0;
        let mut kept_bytes = record.log_bytes;
        let mut kept_text = record.output_bytes;
        if rotate {
            // 一次保留至少整个可容纳的新批次，并给下一批追加留出空间，
            // 避免日志满后每读一块就重写整份日志。
            let discard_previous_tail = incoming_bytes > limit;
            while incoming_bytes > limit {
                incoming_bytes -= incoming.pop_front().expect("non-empty batch").0;
            }
            let budget = (limit / 2).max(incoming_bytes);
            while evict < self.chunks.len()
                && (discard_previous_tail || kept_bytes + incoming_bytes > budget)
            {
                let (size, chunk) = &self.chunks[evict];
                evict += 1;
                kept_bytes -= size;
                kept_text -= chunk.text.len();
            }
        }
        let added_text: usize = incoming.iter().map(|(_, chunk)| chunk.text.len()).sum();
        if rotate {
            store.write_terminal_output(
                &record.id,
                self.chunks
                    .iter()
                    .skip(evict)
                    .chain(incoming.iter())
                    .map(|(_, chunk)| chunk),
                None,
                limit,
            )?;
        } else {
            store.write_terminal_output(
                &record.id,
                incoming.iter().map(|(_, chunk)| chunk),
                Some(record.log_bytes),
                limit,
            )?;
        }
        self.chunks.drain(..evict);
        self.chunks.extend(incoming);
        record.output_chunks = sequence;
        record.log_bytes = kept_bytes + incoming_bytes;
        record.output_bytes = kept_text + added_text;
        record.output_end_byte = Some(end);
        record.output_truncated = end > record.output_bytes as u64;
        record.output_preview =
            summarize_tail(self.chunks.iter().map(|(_, chunk)| chunk), PREVIEW_CHARS);
        Ok(())
    }
}

fn encoded_len(chunk: &TerminalJobOutputChunk) -> Result<usize> {
    Ok(serde_json::to_vec(chunk)?.len() + 1)
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn summarize_tail<'a>(
    chunks: impl DoubleEndedIterator<Item = &'a TerminalJobOutputChunk>,
    max_chars: usize,
) -> String {
    let mut chars = Vec::new();
    for chunk in chunks.rev() {
        let tail = chunk
            .text
            .chars()
            .rev()
            .take(max_chars.saturating_sub(chars.len()));
        chars.extend(tail);
        if chars.len() < max_chars {
            chars.extend(
                "] ".chars()
                    .rev()
                    .chain(chunk.stream.chars().rev())
                    .chain(['['])
                    .take(max_chars - chars.len()),
            );
        }
        if chars.len() == max_chars {
            break;
        }
    }
    chars.into_iter().rev().collect()
}

pub(super) struct OutputLog {
    chunks: Vec<TerminalJobOutputChunk>,
    start: u64,
    end: u64,
}

impl OutputLog {
    pub fn read(store: &Store, id: &str) -> Result<Self> {
        Self::from_chunks(store.list_terminal_output(id)?)
    }

    fn from_chunks(mut chunks: Vec<TerminalJobOutputChunk>) -> Result<Self> {
        // 旧格式只保留前缀，与新格式不同，不知道被丢弃的后缀长度。
        let start = chunks
            .first()
            .and_then(|chunk| chunk.start_byte)
            .unwrap_or(0);
        let mut end = start;
        let mut seq = chunks.first().map_or(0, |chunk| chunk.sequence);
        for chunk in &mut chunks {
            ensure!(
                chunk.sequence == seq && chunk.start_byte.unwrap_or(end) == end,
                "后台日志不连续"
            );
            ensure!(
                matches!(chunk.stream.as_str(), "stdout" | "stderr"),
                "后台日志输出流无效"
            );
            ensure!(!chunk.text.is_empty(), "后台日志含空块");
            chunk.start_byte.get_or_insert(end);
            end = end
                .checked_add(chunk.text.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("后台日志游标超限"))?;
            seq = seq
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("后台日志序号超限"))?;
        }
        Ok(Self { chunks, start, end })
    }

    pub fn reconcile(store: &Store, record: &mut TerminalJobRecord) -> Result<()> {
        let on_disk = store.list_terminal_output(&record.id)?;
        let len = on_disk.iter().try_fold(0, |sum, chunk| {
            Ok::<_, anyhow::Error>(sum + encoded_len(chunk)?)
        })?;
        let has_offsets = on_disk
            .first()
            .is_some_and(|chunk| chunk.start_byte.is_some());
        let log = Self::from_chunks(on_disk)?;
        ensure!(
            record.output_end_byte.unwrap_or(0) <= log.end,
            "后台日志缺失已登记的末尾输出"
        );
        if has_offsets || record.output_end_byte.is_some() {
            record.output_end_byte = Some(log.end);
        }
        record.output_bytes = (log.end - log.start) as usize;
        record.log_bytes = len;
        record.output_chunks = log.chunks.last().map_or(0, |chunk| chunk.sequence + 1);
        record.output_truncated |= log.start > 0;
        record.output_preview = log.tail_text(PREVIEW_CHARS);
        Ok(())
    }

    pub fn validate(&self, record: &TerminalJobRecord) -> Result<()> {
        ensure!(
            record.output_end_byte.is_none_or(|end| end == self.end),
            "后台日志与末尾游标不一致"
        );
        Ok(())
    }

    pub fn tail_text(&self, chars: usize) -> String {
        summarize_tail(self.chunks.iter(), chars)
    }

    pub fn range(&self) -> std::ops::Range<u64> {
        self.start..self.end
    }

    pub fn page(&self, record: &TerminalJobRecord, request: PageRequest) -> Result<OutputPage<'_>> {
        ensure!(
            request.cursor <= self.end,
            "cursor 超出当前输出末尾（{}）",
            self.end
        );
        let mut cursor = request.cursor.max(self.start);
        let mut left = request.limit;
        let mut chunks = Vec::new();
        let first = self
            .chunks
            .partition_point(|chunk| chunk.start_byte.unwrap() + chunk.text.len() as u64 <= cursor);
        for chunk in self.chunks[first..].iter().take(MAX_PAGE_CHUNKS) {
            let start = chunk.start_byte.unwrap();
            let from = (cursor - start) as usize;
            ensure!(
                chunk.text.is_char_boundary(from),
                "cursor 必须指向 UTF-8 字符边界，请使用 nextCursor"
            );
            let text = utf8_prefix(&chunk.text[from..], left);
            if text.is_empty() {
                break;
            }
            chunks.push(PageChunk {
                sequence: chunk.sequence,
                stream: &chunk.stream,
                start_byte: cursor,
                text,
            });
            cursor += text.len() as u64;
            left -= text.len();
            if left == 0 || text.len() < chunk.text.len() - from {
                break;
            }
        }
        Ok(OutputPage {
            retained_start: self.start,
            retained_end: self.end,
            next_cursor: cursor,
            dropped_bytes: if record.output_end_byte.is_none() && record.output_truncated {
                None
            } else {
                Some(self.start)
            },
            skipped_bytes: self.start.saturating_sub(request.cursor),
            output_truncated: record.output_truncated || self.start > 0,
            has_more: cursor < self.end,
            chunks,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct PageRequest {
    pub cursor: u64,
    limit: usize,
    pub wait_ms: u64,
}

pub(super) fn wants_output_page(args: &Value) -> bool {
    ["cursor", "limitBytes", "waitMs"]
        .iter()
        .any(|key| args.get(key).is_some())
}

impl PageRequest {
    pub fn parse(args: &Value) -> Result<Option<Self>> {
        if !wants_output_page(args) {
            return Ok(None);
        }
        let field = |name: &str, default| {
            args.get(name).map_or(Ok(default), |v| {
                v.as_u64()
                    .ok_or_else(|| anyhow::anyhow!("{name} 必须是非负整数"))
            })
        };
        Ok(Some(Self {
            cursor: field("cursor", 0)?,
            limit: field("limitBytes", 4096)?.clamp(4, MAX_PAGE_BYTES) as usize,
            wait_ms: field("waitMs", 0)?.min(MAX_WAIT_MS),
        }))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OutputPage<'a> {
    retained_start: u64,
    retained_end: u64,
    next_cursor: u64,
    dropped_bytes: Option<u64>,
    skipped_bytes: u64,
    output_truncated: bool,
    has_more: bool,
    chunks: Vec<PageChunk<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageChunk<'a> {
    sequence: u64,
    stream: &'a str,
    start_byte: u64,
    text: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_preserves_split_unicode_and_keeps_streams_separate() {
        let crab = "🦀".as_bytes();
        let mut output = OutputTail::default();
        assert_eq!(output.decode(Stream::Stdout, &crab[..3], false), "");
        assert_eq!(output.decode(Stream::Stderr, b"error\n", false), "error\n");
        assert_eq!(output.decode(Stream::Stdout, &crab[3..], false), "🦀");
        assert_eq!(output.decode(Stream::Stderr, &crab[..1], false), "");
        assert_eq!(output.decode(Stream::Stderr, &[], true), "�");
        assert_eq!(output.decode(Stream::Stdout, &[0xfd, b'!'], false), "�!");
    }
}
