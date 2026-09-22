//! A 9x9-first Go engine: the rules, a Monte-Carlo search over them, and the
//! two ways to talk to it — a terminal board and GTP.

pub mod arena;
pub mod board;
pub mod coords;
pub mod game;
pub mod gtp;
pub mod mcts;
pub mod web;
