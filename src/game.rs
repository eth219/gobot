//! A game: the board, plus the history that positional superko and `undo` need.
//!
//! The search works on a bare `Board` with the simple ko rule, because carrying
//! a hash history through a million playouts costs more than it buys. Superko is
//! enforced here, where the moves are real.

use crate::board::{Board, Color, Illegal, Move};

pub struct Game {
    board: Board,
    /// Positions before each move, for `undo`.
    history: Vec<Board>,
    /// Positional hashes of every position the game has stood in.
    seen: Vec<u64>,
    pub komi: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rejected {
    Rules(Illegal),
    Superko,
}

impl Game {
    pub fn new(size: usize, komi: f32) -> Game {
        let board = Board::new(size);
        Game {
            seen: vec![board.hash()],
            history: Vec::new(),
            board,
            komi,
        }
    }

    pub fn from_board(board: Board, komi: f32) -> Game {
        Game {
            seen: vec![board.hash()],
            history: Vec::new(),
            board,
            komi,
        }
    }

    /// A game resumed from a position and the positions it has already stood
    /// in, for a caller that keeps the history itself rather than the `Game` —
    /// a stateless request, a saved game. The position given counts as seen.
    pub fn resumed(board: Board, komi: f32, seen: impl IntoIterator<Item = u64>) -> Game {
        let mut game = Game::from_board(board, komi);
        game.seen.extend(seen);
        game
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    pub fn to_move(&self) -> Color {
        self.board.to_move()
    }

    pub fn set_to_move(&mut self, c: Color) {
        self.board.set_to_move(c);
    }

    pub fn play(&mut self, mv: Move) -> Result<(), Rejected> {
        let before = self.board;
        let mut next = self.board;
        if let Err(why) = next.play(mv) {
            return Err(Rejected::Rules(why));
        }
        if matches!(mv, Move::Play(_)) && self.seen.contains(&next.hash()) {
            return Err(Rejected::Superko);
        }
        self.history.push(before);
        self.seen.push(next.hash());
        self.board = next;
        Ok(())
    }

    /// Adds a stone to the position without it counting as a move, for entering
    /// a game that is already under way.
    pub fn setup_stone(&mut self, pt: crate::board::Point, colour: Color) {
        self.history.push(self.board);
        self.board.setup_stone(pt, colour);
        self.seen.clear();
        self.seen.push(self.board.hash());
    }

    pub fn undo(&mut self) -> bool {
        match self.history.pop() {
            Some(prev) => {
                self.board = prev;
                self.seen.pop();
                if self.seen.is_empty() {
                    self.seen.push(self.board.hash());
                }
                true
            }
            None => false,
        }
    }

    pub fn moves_played(&self) -> usize {
        self.history.len()
    }

    pub fn score(&self) -> f32 {
        self.board.score_area(self.komi)
    }
}
