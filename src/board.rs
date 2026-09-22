//! The board and the rules: stones, chains, captures, ko, and area scoring.
//!
//! The board is a `MAX_SIZE + 2` square with a border ring of sentinel cells, so
//! neighbour lookups never need a bounds check. A `Board` is `Copy` (about half a
//! kilobyte) because the search copies one per playout.

pub const MAX_SIZE: usize = 19;
pub const STRIDE: usize = MAX_SIZE + 2;
pub const NUM_CELLS: usize = STRIDE * STRIDE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Cell {
    Empty = 0,
    Black = 1,
    White = 2,
    Border = 3,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    White = 1,
}

impl Color {
    pub const fn other(self) -> Color {
        match self {
            Color::Black => Color::White,
            Color::White => Color::Black,
        }
    }

    pub const fn cell(self) -> Cell {
        match self {
            Color::Black => Cell::Black,
            Color::White => Cell::White,
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn name(self) -> &'static str {
        match self {
            Color::Black => "black",
            Color::White => "white",
        }
    }
}

/// An index into the padded cell array, not an (x, y) pair.
pub type Point = u16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Move {
    Play(Point),
    Pass,
    Resign,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Illegal {
    NotEmpty,
    Suicide,
    Ko,
    OffBoard,
}

const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Zobrist keys, one per (colour, point). Built at compile time from a fixed
/// seed so that a hash means the same thing in every run and every process.
static ZOBRIST: [[u64; NUM_CELLS]; 2] = {
    let mut table = [[0u64; NUM_CELLS]; 2];
    let mut state = 0x243F_6A88_85A3_08D3;
    let mut c = 0;
    while c < 2 {
        let mut i = 0;
        while i < NUM_CELLS {
            table[c][i] = splitmix64(&mut state);
            i += 1;
        }
        c += 1;
    }
    table
};

const WORDS: usize = NUM_CELLS.div_ceil(64);

/// A set of points, small enough to live on the stack.
#[derive(Clone, Copy)]
struct Bits {
    w: [u64; WORDS],
}

impl Bits {
    const fn new() -> Self {
        Bits { w: [0; WORDS] }
    }
    #[inline]
    fn get(&self, i: usize) -> bool {
        self.w[i >> 6] >> (i & 63) & 1 == 1
    }
    #[inline]
    fn set(&mut self, i: usize) {
        self.w[i >> 6] |= 1 << (i & 63);
    }
    #[inline]
    fn unset(&mut self, i: usize) {
        self.w[i >> 6] &= !(1u64 << (i & 63));
    }
    /// Removes and returns the lowest point in the set. Used as the frontier of
    /// a flood fill: seven words of zeroing beats a 441-entry stack array, and
    /// the fills are the hottest thing in the program.
    #[inline]
    fn take_lowest(&mut self) -> Option<Point> {
        for (k, w) in self.w.iter_mut().enumerate() {
            if *w != 0 {
                let b = w.trailing_zeros() as usize;
                *w &= *w - 1;
                return Some((k * 64 + b) as Point);
            }
        }
        None
    }
    /// The number of points in the set, counting no further than `cap`.
    #[inline]
    fn count(&self, cap: u32) -> u32 {
        let mut n = 0;
        for w in &self.w {
            n += w.count_ones();
            if n >= cap {
                return cap;
            }
        }
        n
    }
}

#[derive(Clone, Copy)]
pub struct Board {
    cells: [Cell; NUM_CELLS],
    size: u8,
    to_move: Color,
    /// The one point the side to move may not retake, from a single-stone capture.
    ko: Option<Point>,
    prisoners: [u32; 2],
    hash: u64,
    passes: u32,
    last_move: Option<Move>,
}

impl Board {
    pub fn new(size: usize) -> Board {
        assert!((2..=MAX_SIZE).contains(&size), "board size out of range");
        let mut cells = [Cell::Border; NUM_CELLS];
        for y in 0..size {
            for x in 0..size {
                cells[(y + 1) * STRIDE + (x + 1)] = Cell::Empty;
            }
        }
        Board {
            cells,
            size: size as u8,
            to_move: Color::Black,
            ko: None,
            prisoners: [0, 0],
            hash: 0,
            passes: 0,
            last_move: None,
        }
    }

    pub const fn size(&self) -> usize {
        self.size as usize
    }
    pub const fn to_move(&self) -> Color {
        self.to_move
    }
    pub fn set_to_move(&mut self, c: Color) {
        self.to_move = c;
        self.ko = None;
    }

    /// Restores the turn and the ko point together on a board that was rebuilt
    /// from stones alone. A position has no memory of the capture that made it,
    /// so anything that reconstructs one — a stateless request, a diagram — has
    /// to be told. Both at once, because setting the turn alone clears the ko.
    pub fn resume(&mut self, c: Color, ko: Option<Point>) {
        self.set_to_move(c);
        self.ko = ko;
    }
    /// Positional hash: stones only, so it identifies a position and not a turn.
    pub const fn hash(&self) -> u64 {
        self.hash
    }
    pub const fn prisoners(&self, c: Color) -> u32 {
        self.prisoners[c.index()]
    }
    pub const fn passes(&self) -> u32 {
        self.passes
    }
    pub const fn last_move(&self) -> Option<Move> {
        self.last_move
    }
    pub const fn ko_point(&self) -> Option<Point> {
        self.ko
    }

    pub const fn point(&self, x: usize, y: usize) -> Point {
        ((y + 1) * STRIDE + (x + 1)) as Point
    }

    /// (x, y) with x rightwards and y upwards from the bottom-left corner.
    pub const fn coords(&self, pt: Point) -> (usize, usize) {
        let i = pt as usize;
        (i % STRIDE - 1, i / STRIDE - 1)
    }

    pub fn cell(&self, pt: Point) -> Cell {
        self.cells[pt as usize]
    }

    pub fn is_on_board(&self, pt: Point) -> bool {
        self.cells[pt as usize] != Cell::Border
    }

    /// Every point of the board, in row-major order from the bottom-left.
    pub fn points(&self) -> impl Iterator<Item = Point> + '_ {
        let size = self.size();
        (0..size).flat_map(move |y| (0..size).map(move |x| ((y + 1) * STRIDE + (x + 1)) as Point))
    }

    const fn neighbours(pt: Point) -> [Point; 4] {
        [pt - 1, pt + 1, pt - STRIDE as Point, pt + STRIDE as Point]
    }

    const fn diagonals(pt: Point) -> [Point; 4] {
        [
            pt - STRIDE as Point - 1,
            pt - STRIDE as Point + 1,
            pt + STRIDE as Point - 1,
            pt + STRIDE as Point + 1,
        ]
    }

    fn put(&mut self, pt: Point, c: Color) {
        self.cells[pt as usize] = c.cell();
        self.hash ^= ZOBRIST[c.index()][pt as usize];
    }

    fn clear(&mut self, pt: Point, c: Color) {
        self.cells[pt as usize] = Cell::Empty;
        self.hash ^= ZOBRIST[c.index()][pt as usize];
    }

    /// How many liberties has the chain at `pt`? Stops counting at `cap`, so
    /// `chain_liberties(pt, 2) == 1` means "in atari".
    pub fn chain_liberties(&self, pt: Point, cap: u32) -> u32 {
        let who = self.cells[pt as usize];
        debug_assert!(who == Cell::Black || who == Cell::White);
        let mut seen = Bits::new();
        let mut counted = Bits::new();
        let mut pending = Bits::new();
        seen.set(pt as usize);
        pending.set(pt as usize);
        let mut libs = 0;
        while let Some(p) = pending.take_lowest() {
            for n in Self::neighbours(p) {
                let i = n as usize;
                let c = self.cells[i];
                if c == Cell::Empty {
                    if !counted.get(i) {
                        counted.set(i);
                        libs += 1;
                        if libs >= cap {
                            return libs;
                        }
                    }
                } else if c == who && !seen.get(i) {
                    seen.set(i);
                    pending.set(i);
                }
            }
        }
        libs
    }

    /// Adds the liberties of the chain at `pt` to `out`.
    fn chain_liberties_into(&self, pt: Point, out: &mut Bits) {
        let who = self.cells[pt as usize];
        let mut seen = Bits::new();
        let mut pending = Bits::new();
        seen.set(pt as usize);
        pending.set(pt as usize);
        while let Some(p) = pending.take_lowest() {
            for n in Self::neighbours(p) {
                let i = n as usize;
                let c = self.cells[i];
                if c == Cell::Empty {
                    out.set(i);
                } else if c == who && !seen.get(i) {
                    seen.set(i);
                    pending.set(i);
                }
            }
        }
    }

    /// The one liberty of a chain in atari, or `None` if it has more or fewer.
    pub fn chain_single_liberty(&self, pt: Point) -> Option<Point> {
        let who = self.cells[pt as usize];
        let mut seen = Bits::new();
        let mut pending = Bits::new();
        seen.set(pt as usize);
        pending.set(pt as usize);
        let mut found: Option<Point> = None;
        while let Some(p) = pending.take_lowest() {
            for n in Self::neighbours(p) {
                let i = n as usize;
                let c = self.cells[i];
                if c == Cell::Empty {
                    match found {
                        None => found = Some(n),
                        Some(f) if f != n => return None,
                        _ => {}
                    }
                } else if c == who && !seen.get(i) {
                    seen.set(i);
                    pending.set(i);
                }
            }
        }
        found
    }

    /// The size of the chain at `pt`, in stones.
    pub fn chain_size(&self, pt: Point) -> u32 {
        let who = self.cells[pt as usize];
        let mut seen = Bits::new();
        let mut pending = Bits::new();
        seen.set(pt as usize);
        pending.set(pt as usize);
        let mut n_stones = 0;
        while let Some(p) = pending.take_lowest() {
            n_stones += 1;
            for n in Self::neighbours(p) {
                let i = n as usize;
                if self.cells[i] == who && !seen.get(i) {
                    seen.set(i);
                    pending.set(i);
                }
            }
        }
        n_stones
    }

    /// Lifts the chain at `pt` off the board and returns how many stones went.
    /// The fill is destructive, so it needs no visited set.
    fn remove_chain(&mut self, pt: Point) -> u32 {
        let who = self.cells[pt as usize];
        let colour = if who == Cell::Black {
            Color::Black
        } else {
            Color::White
        };
        let mut pending = Bits::new();
        pending.set(pt as usize);
        self.clear(pt, colour);
        let mut removed = 1;
        while let Some(p) = pending.take_lowest() {
            for n in Self::neighbours(p) {
                if self.cells[n as usize] == who {
                    self.clear(n, colour);
                    removed += 1;
                    pending.set(n as usize);
                }
            }
        }
        removed
    }

    /// Would `colour` playing at `pt` be legal? Checks emptiness, the ko point
    /// and suicide, in that order, without touching the board.
    pub fn is_legal(&self, pt: Point, colour: Color) -> bool {
        if self.cells[pt as usize] != Cell::Empty {
            return false;
        }
        if self.ko == Some(pt) {
            return false;
        }
        let mine = colour.cell();
        let theirs = colour.other().cell();
        for n in Self::neighbours(pt) {
            match self.cells[n as usize] {
                // An empty neighbour is a liberty, so the stone lives.
                Cell::Empty => return true,
                // Joining a chain that keeps a liberty of its own.
                c if c == mine => {
                    if self.chain_liberties(n, 2) > 1 {
                        return true;
                    }
                }
                // Capturing leaves the freed point as a liberty.
                c if c == theirs && self.chain_liberties(n, 2) == 1 => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    pub fn illegal_reason(&self, pt: Point, colour: Color) -> Option<Illegal> {
        if !self.is_on_board(pt) {
            return Some(Illegal::OffBoard);
        }
        if self.cells[pt as usize] != Cell::Empty {
            return Some(Illegal::NotEmpty);
        }
        if self.ko == Some(pt) {
            return Some(Illegal::Ko);
        }
        if !self.is_legal(pt, colour) {
            return Some(Illegal::Suicide);
        }
        None
    }

    /// Is `pt` an eye-shaped point for `colour`? Used to stop playouts from
    /// filling their own eyes, which is what makes a random game terminate.
    /// Orthogonals must all be friendly or off-board; at most one hostile
    /// diagonal is tolerated, and none at the edge.
    pub fn is_eyelike(&self, pt: Point, colour: Color) -> bool {
        if self.cells[pt as usize] != Cell::Empty {
            return false;
        }
        let mine = colour.cell();
        for n in Self::neighbours(pt) {
            let c = self.cells[n as usize];
            if c != mine && c != Cell::Border {
                return false;
            }
        }
        let theirs = colour.other().cell();
        let mut hostile = 0;
        let mut at_edge = false;
        for d in Self::diagonals(pt) {
            match self.cells[d as usize] {
                Cell::Border => at_edge = true,
                c if c == theirs => hostile += 1,
                _ => {}
            }
        }
        hostile == 0 || (hostile == 1 && !at_edge)
    }

    /// Does playing at `pt` put the resulting chain in atari without capturing?
    /// Cheap to ask: two empty neighbours already rule it out, so the liberty
    /// union below only runs for the handful of points that could qualify.
    pub fn is_self_atari(&self, pt: Point, colour: Color) -> bool {
        let mine = colour.cell();
        let theirs = colour.other().cell();
        let mut empties = 0;
        for n in Self::neighbours(pt) {
            if self.cells[n as usize] == Cell::Empty {
                empties += 1;
            }
        }
        if empties >= 2 {
            return false;
        }
        // Anything that captures gains the freed points as liberties, and is a
        // sacrifice the playout policy has no business vetoing.
        for n in Self::neighbours(pt) {
            if self.cells[n as usize] == theirs && self.chain_liberties(n, 2) == 1 {
                return false;
            }
        }
        // The liberties of the chain the new stone would belong to: its own
        // empty neighbours plus those of every friendly chain it joins, less
        // the point it is about to fill.
        let mut libs = Bits::new();
        for n in Self::neighbours(pt) {
            match self.cells[n as usize] {
                Cell::Empty => libs.set(n as usize),
                c if c == mine => self.chain_liberties_into(n, &mut libs),
                _ => {}
            }
        }
        libs.unset(pt as usize);
        libs.count(2) < 2
    }

    /// Places a stone with no legality check and no turn change, for building a
    /// position that a game has already reached. Captures are still resolved.
    pub fn setup_stone(&mut self, pt: Point, colour: Color) {
        if !self.is_on_board(pt) {
            return;
        }
        if self.cells[pt as usize] != Cell::Empty {
            let c = self.cells[pt as usize];
            self.clear(
                pt,
                if c == Cell::Black {
                    Color::Black
                } else {
                    Color::White
                },
            );
        }
        self.put(pt, colour);
        for n in Self::neighbours(pt) {
            if self.cells[n as usize] == colour.other().cell() && self.chain_liberties(n, 1) == 0 {
                self.remove_chain(n);
            }
        }
        self.ko = None;
    }

    pub fn remove_stone(&mut self, pt: Point) {
        match self.cells[pt as usize] {
            Cell::Black => self.clear(pt, Color::Black),
            Cell::White => self.clear(pt, Color::White),
            _ => {}
        }
    }

    pub fn play(&mut self, mv: Move) -> Result<(), Illegal> {
        match mv {
            Move::Pass | Move::Resign => {
                self.ko = None;
                self.passes += 1;
                self.to_move = self.to_move.other();
                self.last_move = Some(mv);
                Ok(())
            }
            Move::Play(pt) => {
                if let Some(why) = self.illegal_reason(pt, self.to_move) {
                    return Err(why);
                }
                let colour = self.to_move;
                self.put(pt, colour);
                let theirs = colour.other().cell();
                let mut captured = 0;
                let mut last_captured = 0u16;
                for n in Self::neighbours(pt) {
                    if self.cells[n as usize] == theirs && self.chain_liberties(n, 1) == 0 {
                        last_captured = n;
                        captured += self.remove_chain(n);
                    }
                }
                self.prisoners[colour.index()] += captured;
                // A ko is a one-for-one exchange: one stone taken by a lone
                // stone that itself has a single liberty.
                self.ko = if captured == 1
                    && self.chain_size(pt) == 1
                    && self.chain_liberties(pt, 2) == 1
                {
                    Some(last_captured)
                } else {
                    None
                };
                self.passes = 0;
                self.to_move = colour.other();
                self.last_move = Some(mv);
                Ok(())
            }
        }
    }

    /// Every legal move that is not filling one's own eye, plus `Pass` last.
    pub fn candidate_moves(&self, colour: Color, out: &mut Vec<Move>) {
        out.clear();
        for pt in self.points() {
            if self.cells[pt as usize] == Cell::Empty
                && !self.is_eyelike(pt, colour)
                && self.is_legal(pt, colour)
            {
                out.push(Move::Play(pt));
            }
        }
        out.push(Move::Pass);
    }

    /// Area score under Tromp-Taylor: stones plus the empty regions that touch
    /// exactly one colour. Positive means Black is ahead.
    pub fn score_area(&self, komi: f32) -> f32 {
        let mut black = 0i32;
        let mut white = 0i32;
        let mut done = Bits::new();
        for pt in self.points() {
            let i = pt as usize;
            match self.cells[i] {
                Cell::Black => black += 1,
                Cell::White => white += 1,
                Cell::Empty => {
                    if done.get(i) {
                        continue;
                    }
                    // Flood the empty region and see whose stones enclose it.
                    let mut pending = Bits::new();
                    pending.set(i);
                    done.set(i);
                    let mut n_region = 0i32;
                    let mut touches_black = false;
                    let mut touches_white = false;
                    while let Some(p) = pending.take_lowest() {
                        n_region += 1;
                        for n in Self::neighbours(p) {
                            let j = n as usize;
                            match self.cells[j] {
                                Cell::Empty => {
                                    if !done.get(j) {
                                        done.set(j);
                                        pending.set(j);
                                    }
                                }
                                Cell::Black => touches_black = true,
                                Cell::White => touches_white = true,
                                Cell::Border => {}
                            }
                        }
                    }
                    match (touches_black, touches_white) {
                        (true, false) => black += n_region,
                        (false, true) => white += n_region,
                        _ => {}
                    }
                }
                Cell::Border => {}
            }
        }
        black as f32 - white as f32 - komi
    }

    /// The score a playout ends on: stones plus single-colour-adjacent empties.
    /// At the end of a playout the board is filled but for eyes, so this agrees
    /// with `score_area` there while costing one pass over the points.
    pub fn score_playout(&self, komi: f32) -> f32 {
        let mut black = 0i32;
        let mut white = 0i32;
        for pt in self.points() {
            match self.cells[pt as usize] {
                Cell::Black => black += 1,
                Cell::White => white += 1,
                Cell::Empty => {
                    let mut b = false;
                    let mut w = false;
                    for n in Self::neighbours(pt) {
                        match self.cells[n as usize] {
                            Cell::Black => b = true,
                            Cell::White => w = true,
                            _ => {}
                        }
                    }
                    match (b, w) {
                        (true, false) => black += 1,
                        (false, true) => white += 1,
                        _ => {}
                    }
                }
                Cell::Border => {}
            }
        }
        black as f32 - white as f32 - komi
    }
}
