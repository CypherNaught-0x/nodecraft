//! Bounded RFC 4180-style input records, including quoted commas/newlines and escaped quotes.
use nodecraft_types::{Error, ErrorKind};
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ROWS: usize = 1000;
const MAX_COLUMNS: usize = 256;
const MAX_CELL: usize = 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    Plain,
    Quoted,
    AfterQuote,
}
fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
pub fn parse(text: &str) -> Result<Vec<Vec<String>>, Error> {
    if text.len() > MAX_BYTES {
        return Err(invalid("CSV exceeds 8 MiB"));
    }
    let mut state = State::Start;
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut content = false;
    let mut chars = text.strip_prefix('\u{feff}').unwrap_or(text).chars().peekable();
    while let Some(c) = chars.next() {
        if state == State::Quoted {
            if c == '"' {
                state = State::AfterQuote;
            } else {
                push(&mut cell, c)?;
            }
            continue;
        }
        if state == State::AfterQuote && c == '"' {
            push(&mut cell, c)?;
            state = State::Quoted;
            continue;
        }
        if c == ',' {
            field(&mut row, &mut cell)?;
            state = State::Start;
            content = true;
            continue;
        }
        if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if content || !row.is_empty() {
                field(&mut row, &mut cell)?;
                record(&mut rows, &mut row)?;
            }
            state = State::Start;
            content = false;
            continue;
        }
        match state {
            State::Start if c == '"' => {
                state = State::Quoted;
                content = true;
            }
            State::AfterQuote => return Err(invalid("unexpected text after a quoted CSV field")),
            _ if c == '"' => return Err(invalid("a CSV quote must start a field")),
            _ => {
                push(&mut cell, c)?;
                state = State::Plain;
                content = true;
            }
        }
    }
    if state == State::Quoted {
        return Err(invalid("unterminated quoted CSV field"));
    }
    if content || !row.is_empty() {
        field(&mut row, &mut cell)?;
        record(&mut rows, &mut row)?;
    }
    Ok(rows)
}
fn push(cell: &mut String, c: char) -> Result<(), Error> {
    if cell.len().saturating_add(c.len_utf8()) > MAX_CELL {
        return Err(invalid("one CSV field exceeds 1 MiB"));
    }
    cell.push(c);
    Ok(())
}
fn field(row: &mut Vec<String>, cell: &mut String) -> Result<(), Error> {
    if row.len() >= MAX_COLUMNS {
        return Err(invalid("CSV has more than 256 columns"));
    }
    row.push(std::mem::take(cell));
    Ok(())
}
fn record(rows: &mut Vec<Vec<String>>, row: &mut Vec<String>) -> Result<(), Error> {
    if rows.len() > MAX_ROWS {
        return Err(invalid("CSV has more than 1000 data rows"));
    }
    rows.push(std::mem::take(row));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli97_csv_preserves_quoted_commas_newlines_quotes_unicode_and_empty_cells() {
        assert_eq!(
            parse("\u{feff}prompt,size\r\n\"a,b\n\"\"quoted\"\"\",雪\r\nlast,\n").unwrap(),
            vec![vec!["prompt", "size"], vec!["a,b\n\"quoted\"", "雪"], vec!["last", ""]]
        );
        assert_eq!(parse("x\n\"\"\n\n").unwrap(), vec![vec!["x"], vec![""]]);
    }
    #[test]
    fn cli97_csv_rejects_unbalanced_quotes_trailing_junk_and_bounded_extents() {
        for text in ["\"unfinished", "un\"quoted", "\"closed\"junk"] {
            assert!(parse(text).is_err());
        }
        assert!(parse(&"x".repeat(MAX_CELL + 1)).is_err());
        assert!(parse(&"x,".repeat(MAX_COLUMNS)).is_err());
        let exact = format!("header\n{}", "row\n".repeat(MAX_ROWS));
        assert_eq!(parse(&exact).unwrap().len(), MAX_ROWS + 1);
        assert!(parse(&(exact + "row\n")).is_err());
        assert!(parse(&"x".repeat(MAX_BYTES + 1)).is_err());
    }
}
