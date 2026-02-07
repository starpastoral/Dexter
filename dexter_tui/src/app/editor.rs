use ratatui::layout::Rect;

pub fn split_line_at_char(line: &str, idx: usize) -> (String, Option<char>, String) {
    let mut before = String::new();
    let mut current = None;
    let mut after = String::new();

    for (i, ch) in line.chars().enumerate() {
        if i < idx {
            before.push(ch);
        } else if i == idx {
            current = Some(ch);
        } else {
            after.push(ch);
        }
    }

    (before, current, after)
}

pub fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn byte_index(text: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    if let Some((idx, _)) = text.char_indices().nth(char_idx) {
        return idx;
    }
    text.len()
}

pub fn insert_char_at_cursor(text: &mut String, cursor: &mut usize, ch: char) {
    let idx = byte_index(text, *cursor);
    text.insert(idx, ch);
    *cursor += 1;
}

pub fn delete_char_before_cursor(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = byte_index(text, *cursor - 1);
    let end = byte_index(text, *cursor);
    if start < end {
        text.replace_range(start..end, "");
        *cursor -= 1;
    }
}

pub fn delete_char_at_cursor(text: &mut String, cursor: &mut usize) {
    let len = char_count(text);
    if *cursor >= len {
        return;
    }
    let start = byte_index(text, *cursor);
    let end = byte_index(text, *cursor + 1);
    if start < end {
        text.replace_range(start..end, "");
    }
}

fn line_lengths(text: &str) -> Vec<usize> {
    let mut lines: Vec<usize> = text.split('\n').map(|line| line.chars().count()).collect();
    if lines.is_empty() {
        lines.push(0);
    }
    lines
}

fn cursor_line_col(line_lens: &[usize], cursor: usize) -> (usize, usize) {
    let mut remaining = cursor;
    for (i, len) in line_lens.iter().enumerate() {
        if remaining <= *len {
            return (i, remaining);
        }
        remaining = remaining.saturating_sub(len + 1);
    }
    let last = line_lens.len().saturating_sub(1);
    let last_len = *line_lens.get(last).unwrap_or(&0);
    (last, last_len)
}

fn cursor_from_line_col(line_lens: &[usize], line_idx: usize, col: usize) -> usize {
    let mut idx = 0usize;
    for i in 0..line_idx {
        idx = idx.saturating_add(line_lens.get(i).copied().unwrap_or(0) + 1);
    }
    let line_len = line_lens.get(line_idx).copied().unwrap_or(0);
    idx.saturating_add(col.min(line_len))
}

pub fn move_cursor_up(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, col) = cursor_line_col(&line_lens, *cursor);
    if line == 0 {
        *cursor = cursor_from_line_col(&line_lens, 0, col);
        return;
    }
    *cursor = cursor_from_line_col(&line_lens, line - 1, col);
}

pub fn move_cursor_down(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, col) = cursor_line_col(&line_lens, *cursor);
    if line + 1 >= line_lens.len() {
        *cursor = cursor_from_line_col(&line_lens, line, col);
        return;
    }
    *cursor = cursor_from_line_col(&line_lens, line + 1, col);
}

pub fn move_cursor_line_start(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, _) = cursor_line_col(&line_lens, *cursor);
    *cursor = cursor_from_line_col(&line_lens, line, 0);
}

pub fn move_cursor_line_end(text: &str, cursor: &mut usize) {
    let line_lens = line_lengths(text);
    let (line, _) = cursor_line_col(&line_lens, *cursor);
    let line_len = line_lens.get(line).copied().unwrap_or(0);
    *cursor = cursor_from_line_col(&line_lens, line, line_len);
}

pub fn point_in_rect(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

pub fn set_cursor_from_click(text: &str, cursor: &mut usize, area: Rect, col: u16, row: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let row_in_area = row.saturating_sub(area.y) as usize;
    let col_in_area = col.saturating_sub(area.x) as usize;
    let text_row = row_in_area.saturating_sub(1);
    let lines: Vec<&str> = text.split('\n').collect();
    let line_idx = text_row.min(lines.len().saturating_sub(1));
    let line = lines.get(line_idx).copied().unwrap_or("");

    let prefix_len = 3usize;
    let col_in_text = col_in_area.saturating_sub(prefix_len);
    let target_col = col_in_text.min(line.chars().count());

    let line_lens = line_lengths(text);
    *cursor = cursor_from_line_col(&line_lens, line_idx, target_col);
}
