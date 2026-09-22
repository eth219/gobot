//! Vertex names and board diagrams: the two ways a position gets in and out.
//!
//! Columns are lettered A..T with I left out, the way every Go program and every
//! Go app labels them; rows count up from 1 at the bottom.

use crate::board::{Board, Cell, Color, Move, Point};

const LETTERS: &[u8] = b"ABCDEFGHJKLMNOPQRST";

pub fn column_letter(x: usize) -> char {
    LETTERS[x] as char
}

pub fn format_move(board: &Board, mv: Move) -> String {
    match mv {
        Move::Pass => "pass".to_string(),
        Move::Resign => "resign".to_string(),
        Move::Play(pt) => {
            let (x, y) = board.coords(pt);
            format!("{}{}", column_letter(x), y + 1)
        }
    }
}

/// Parses `d4`, `D4`, `pass` or `resign`. Case does not matter and `i` is
/// rejected rather than guessed at, because a column `I` does not exist.
pub fn parse_move(board: &Board, text: &str) -> Option<Move> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("pass") {
        return Some(Move::Pass);
    }
    if t.eq_ignore_ascii_case("resign") {
        return Some(Move::Resign);
    }
    let bytes = t.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let letter = bytes[0].to_ascii_uppercase();
    let x = LETTERS.iter().position(|&c| c == letter)?;
    let row: usize = t[1..].trim().parse().ok()?;
    if x >= board.size() || row == 0 || row > board.size() {
        return None;
    }
    Some(Move::Play(board.point(x, row - 1)))
}

/// A diagram of the board, row `size` at the top, so that it matches what a Go
/// app shows. `marks` are highlighted with brackets.
pub fn diagram(board: &Board, marks: &[Point]) -> String {
    let size = board.size();
    let mut s = String::new();
    let width = if size >= 10 { 2 } else { 1 };
    s.push_str(&" ".repeat(width + 1));
    for x in 0..size {
        s.push(column_letter(x));
        s.push(' ');
    }
    s.push('\n');
    for y in (0..size).rev() {
        s.push_str(&format!("{:>width$} ", y + 1, width = width));
        for x in 0..size {
            let pt = board.point(x, y);
            let marked = marks.contains(&pt);
            let glyph = match board.cell(pt) {
                Cell::Black => 'X',
                Cell::White => 'O',
                // A marked empty point is the one being recommended, and `(.)`
                // reads as nothing at all.
                _ if marked => '*',
                _ if is_star_point(size, x, y) => '+',
                _ => '.',
            };
            if marked {
                // Overwrite the trailing space of the previous column.
                if s.ends_with(' ') {
                    s.pop();
                }
                s.push('(');
                s.push(glyph);
                s.push(')');
            } else {
                s.push(glyph);
                s.push(' ');
            }
        }
        s.push_str(&format!("{:>width$}", y + 1, width = width));
        s.push('\n');
    }
    s.push_str(&" ".repeat(width + 1));
    for x in 0..size {
        s.push(column_letter(x));
        s.push(' ');
    }
    s.push('\n');
    s
}

fn is_star_point(size: usize, x: usize, y: usize) -> bool {
    let edge = if size >= 13 { 3 } else { 2 };
    let mid = size / 2;
    let on = |v: usize| v == edge || v == size - 1 - edge || (size % 2 == 1 && v == mid);
    if size < 7 {
        return false;
    }
    on(x) && on(y)
}

/// Reads a position from a diagram. `X`, `x`, `#`, `b`, `B` are black stones,
/// `O`, `o`, `w`, `W` are white, anything else on a stone row is empty. Rows are
/// taken top-first; column letter and row number labels are ignored. Returns the
/// board, or an error naming what did not line up.
pub fn parse_diagram(text: &str, size: usize) -> Result<Board, String> {
    parse_diagram_report(text, size).map(|(board, _)| board)
}

/// As `parse_diagram`, and also the points whose stones did not survive.
///
/// A stone with no liberties cannot be on a board, so one that is typed in gets
/// taken off — which is right, and silent, and means a single mis-copied point
/// turns the position into a different one without saying so. The caller is
/// expected to tell whoever typed it.
pub fn parse_diagram_report(text: &str, size: usize) -> Result<(Board, Vec<Point>), String> {
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for line in text.lines() {
        // A line is a row of stones only if everything on it, once the labels
        // and decoration are dropped, is a stone glyph. That is what keeps the
        // column-letter header out: `B` is a black stone but `A` is not a
        // glyph at all, so the whole line is rejected rather than half-read.
        let mut row = Vec::new();
        let mut all_glyphs = true;
        for ch in line.chars() {
            match ch {
                'X' | 'x' | '#' | 'B' | 'b' | '@' => row.push(Cell::Black),
                'O' | 'o' | 'W' | 'w' => row.push(Cell::White),
                '.' | '+' | '-' | '*' | ',' => row.push(Cell::Empty),
                c if c.is_whitespace() || c.is_ascii_digit() => {}
                '(' | ')' | '[' | ']' | '|' => {}
                _ => all_glyphs = false,
            }
        }
        if all_glyphs && !row.is_empty() {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        return Err("no stone rows found in the diagram".to_string());
    }
    if rows.len() != size {
        return Err(format!(
            "the diagram has {} rows, but the board is {}x{}",
            rows.len(),
            size,
            size
        ));
    }
    let mut board = Board::new(size);
    let mut intended: Vec<(Point, Cell)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if row.len() != size {
            return Err(format!(
                "row {} of the diagram has {} points, expected {}",
                i + 1,
                row.len(),
                size
            ));
        }
        let y = size - 1 - i;
        for (x, cell) in row.iter().enumerate() {
            let pt = board.point(x, y);
            match cell {
                Cell::Black => board.setup_stone(pt, Color::Black),
                Cell::White => board.setup_stone(pt, Color::White),
                _ => continue,
            }
            intended.push((pt, *cell));
        }
    }
    let dropped = intended
        .into_iter()
        .filter(|&(pt, cell)| board.cell(pt) != cell)
        .map(|(pt, _)| pt)
        .collect();
    Ok((board, dropped))
}

/// The conventional handicap points for a board of this size, in the order the
/// stones go down: two corners, then the other two, then the sides, with the
/// centre filling each odd count. Even boards and boards under 7 have no centre
/// or no star points, so they stop at four.
pub fn handicap_points(size: usize, stones: usize) -> Result<Vec<(usize, usize)>, String> {
    if !(2..=9).contains(&stones) {
        return Err("handicap is 2 to 9 stones".to_string());
    }
    if size < 7 {
        return Err(format!("a {size}x{size} board has no handicap points"));
    }
    let edge = if size >= 13 { 3 } else { 2 };
    let far = size - 1 - edge;
    let mid = size / 2;
    let has_centre = size % 2 == 1;
    if stones > 4 && !has_centre {
        return Err(format!(
            "a {size}x{size} board has no centre point, so at most 4 stones"
        ));
    }
    let corners = [(edge, edge), (far, far), (edge, far), (far, edge)];
    let sides = [(edge, mid), (far, mid), (mid, edge), (mid, far)];
    let centre = (mid, mid);
    let mut out: Vec<(usize, usize)> = Vec::new();
    match stones {
        2 => out.extend(&corners[..2]),
        3 => out.extend(&corners[..3]),
        4 => out.extend(&corners[..4]),
        5 => {
            out.extend(&corners[..4]);
            out.push(centre);
        }
        6 => {
            out.extend(&corners[..4]);
            out.extend(&sides[..2]);
        }
        7 => {
            out.extend(&corners[..4]);
            out.extend(&sides[..2]);
            out.push(centre);
        }
        8 => {
            out.extend(&corners[..4]);
            out.extend(&sides[..4]);
        }
        _ => {
            out.extend(&corners[..4]);
            out.extend(&sides[..4]);
            out.push(centre);
        }
    }
    Ok(out)
}
