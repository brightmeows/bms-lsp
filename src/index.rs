//! UTF-8 字节偏移与 LSP UTF-16 位置互转。

use lsp_types::Position;

/// 文档行索引。
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// 每行起始字节偏移。
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// 从文本构建行索引。
    #[must_use]
    pub fn new(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut line_starts = vec![0usize];
        let mut index = 0usize;
        while index < bytes.len() {
            match bytes.get(index) {
                Some(b'\n') => {
                    index += 1;
                    line_starts.push(index);
                }
                Some(b'\r') => {
                    if bytes.get(index + 1) == Some(&b'\n') {
                        index += 2;
                    } else {
                        index += 1;
                    }
                    line_starts.push(index);
                }
                _ => index += 1,
            }
        }
        Self { line_starts }
    }

    /// 把字节偏移转换为 LSP 位置。
    #[must_use]
    pub fn position(&self, text: &str, offset: usize) -> Position {
        let line = self
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        let character = text
            .get(start..offset)
            .map_or(0, |slice| slice.encode_utf16().count());
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: u32::try_from(character).unwrap_or(u32::MAX),
        }
    }

    /// 把 LSP 位置转换为字节偏移。
    #[must_use]
    pub fn offset(&self, text: &str, position: Position) -> Option<usize> {
        let line = usize::try_from(position.line).ok()?;
        let start = *self.line_starts.get(line)?;
        let end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(text.len());
        let line_text = text.get(start..end)?;
        let target = usize::try_from(position.character).ok()?;
        let mut units = 0usize;
        for (offset, ch) in line_text.char_indices() {
            if units >= target {
                return Some(start + offset);
            }
            units += ch.len_utf16();
        }
        Some(start + line_text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;
    use lsp_types::Position;

    /// `#TITLE ` 后的字节偏移。
    const TITLE_VALUE_OFFSET: usize = 7;

    #[test]
    fn position_maps_ascii_prefix() {
        let text = "#TITLE Song\n";
        let index = LineIndex::new(text);
        assert_eq!(
            index.position(text, TITLE_VALUE_OFFSET),
            Position {
                line: 0,
                character: 7
            }
        );
    }

    #[test]
    fn offset_maps_second_line() {
        let text = "#TITLE Song\n#BPM 180\n";
        let index = LineIndex::new(text);
        let offset = text.find("#BPM").unwrap_or_default();
        assert_eq!(
            index.offset(
                text,
                Position {
                    line: 1,
                    character: 0
                }
            ),
            Some(offset)
        );
    }

    #[test]
    fn japanese_text_counts_utf16_units() {
        let text = "#TITLE テスト\n";
        let index = LineIndex::new(text);
        assert_eq!(
            index.position(text, text.find('\n').unwrap_or_default()),
            Position {
                line: 0,
                character: 10
            }
        );
    }
}
