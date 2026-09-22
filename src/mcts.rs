//! Monte-Carlo tree search with RAVE, and the playout policy under it.
//!
//! One tree per thread, all rooted at the same position, and the root move
//! statistics are summed at the end. That is root parallelisation: no locks, no
//! shared mutable state, and each tree stays honest on its own.

use std::time::{Duration, Instant};

use crate::board::{Board, Cell, Color, Move, Point};

const NONE: u32 = u32::MAX;

/// splitmix64. Small, fast, and the same sequence for the same seed, which is
/// what makes a search reproducible when `threads` and `playouts` are fixed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    #[inline]
    pub fn below(&mut self, n: usize) -> usize {
        // Multiply-shift: no division, and the bias is far below anything a
        // playout policy could notice.
        ((self.next_u64() >> 32) as usize * n) >> 32
    }
    #[inline]
    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent as usize
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub komi: f32,
    /// UCT exploration weight, against a win rate in [0, 1].
    pub uct_c: f32,
    /// RAVE's b in Silver's beta; larger trusts RAVE for longer.
    pub rave_b: f32,
    /// Visits a node must have before its children are given nodes of their own.
    pub expand_threshold: u32,
    pub max_nodes: usize,
    pub threads: usize,
    pub time_limit: Option<Duration>,
    pub playouts: Option<u64>,
    /// How often a playout answers an atari instead of playing at random.
    pub atari_answer_percent: u32,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            komi: 7.5,
            uct_c: 0.7,
            rave_b: 0.03,
            expand_threshold: 16,
            max_nodes: 400_000,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get().min(8)),
            time_limit: Some(Duration::from_secs(3)),
            playouts: None,
            atari_answer_percent: 80,
            seed: 0x5EED,
        }
    }
}

#[derive(Clone, Copy)]
struct Edge {
    mv: Move,
    child: u32,
    rave_visits: u32,
    rave_wins: u32,
}

struct Node {
    to_move: Color,
    visits: u32,
    /// Playouts won by the player who moved *into* this node.
    wins: u32,
    edges: Vec<Edge>,
}

struct Tree {
    nodes: Vec<Node>,
    params: Params,
    rng: Rng,
    playouts: u64,
    // Reused across iterations so an iteration allocates nothing.
    path: Vec<(u32, u32, u32)>,
    seq: Vec<(Point, Color)>,
    empties: Vec<Point>,
}

const WORDS: usize = crate::board::NUM_CELLS.div_ceil(64);

#[derive(Clone, Copy)]
struct Bits {
    w: [u64; WORDS],
}

impl Bits {
    const fn new() -> Bits {
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
}

impl Tree {
    fn new(root: &Board, params: Params, seed: u64) -> Tree {
        Tree {
            nodes: vec![Node {
                to_move: root.to_move(),
                visits: 0,
                wins: 0,
                edges: Vec::new(),
            }],
            params,
            rng: Rng::new(seed),
            playouts: 0,
            path: Vec::with_capacity(64),
            seq: Vec::with_capacity(512),
            empties: Vec::with_capacity(crate::board::NUM_CELLS),
        }
    }

    fn expand(&mut self, node: u32, board: &Board) {
        let colour = self.nodes[node as usize].to_move;
        let mut moves = Vec::new();
        board.candidate_moves(colour, &mut moves);
        // Shuffle so that equal-valued moves are not always broken towards A1.
        for i in (1..moves.len()).rev() {
            let j = self.rng.below(i + 1);
            moves.swap(i, j);
        }
        let edges = moves
            .into_iter()
            .map(|mv| Edge {
                mv,
                child: NONE,
                rave_visits: 0,
                rave_wins: 0,
            })
            .collect();
        self.nodes[node as usize].edges = edges;
    }

    fn select(&self, node: u32) -> usize {
        let n = &self.nodes[node as usize];
        let log_parent = ((n.visits.max(1)) as f32).ln();
        let b = self.params.rave_b;
        let mut best = 0usize;
        let mut best_value = f32::NEG_INFINITY;
        for (i, e) in n.edges.iter().enumerate() {
            let (visits, q) = if e.child == NONE {
                (0.0f32, 0.5f32)
            } else {
                let c = &self.nodes[e.child as usize];
                if c.visits == 0 {
                    (0.0, 0.5)
                } else {
                    (c.visits as f32, c.wins as f32 / c.visits as f32)
                }
            };
            let rv = e.rave_visits as f32;
            let value = if rv > 0.0 || visits > 0.0 {
                let q_rave = if rv > 0.0 {
                    e.rave_wins as f32 / rv
                } else {
                    0.0
                };
                let beta = if rv > 0.0 {
                    rv / (rv + visits + 4.0 * rv * visits * b * b)
                } else {
                    0.0
                };
                (1.0 - beta) * q + beta * q_rave
            } else {
                0.5
            } + self.params.uct_c * (log_parent / (visits + 1.0)).sqrt();
            if value > best_value {
                best_value = value;
                best = i;
            }
        }
        best
    }

    /// One simulation: descend, expand if the leaf is warm enough, play out,
    /// then carry the result back up both the tree and the RAVE statistics.
    fn iterate(&mut self, root: &Board) {
        let mut board = *root;
        self.path.clear();
        self.seq.clear();
        let mut node = 0u32;
        loop {
            if self.nodes[node as usize].edges.is_empty() {
                let warm = self.nodes[node as usize].visits >= self.params.expand_threshold;
                let room = self.nodes.len() < self.params.max_nodes;
                if (node == 0 || warm) && room {
                    self.expand(node, &board);
                }
                if self.nodes[node as usize].edges.is_empty() {
                    break;
                }
            }
            if board.passes() >= 2 {
                break;
            }
            let ei = self.select(node);
            let mv = self.nodes[node as usize].edges[ei].mv;
            let pre_len = self.seq.len() as u32;
            let mover = self.nodes[node as usize].to_move;
            if board.play(mv).is_err() {
                // The position moved on under an edge built for an earlier one
                // (a ko point, say). Drop the edge and try again next time.
                self.nodes[node as usize].edges.swap_remove(ei);
                break;
            }
            if let Move::Play(pt) = mv {
                self.seq.push((pt, mover));
            }
            self.path.push((node, ei as u32, pre_len));
            let child = self.nodes[node as usize].edges[ei].child;
            if child == NONE {
                let new = self.nodes.len() as u32;
                self.nodes.push(Node {
                    to_move: board.to_move(),
                    visits: 0,
                    wins: 0,
                    edges: Vec::new(),
                });
                self.nodes[node as usize].edges[ei].child = new;
                break;
            }
            node = child;
        }

        let winner = playout(
            &mut board,
            &mut self.rng,
            &mut self.seq,
            &mut self.empties,
            &self.params,
        );
        self.playouts += 1;
        self.backup(winner);
    }

    fn backup(&mut self, winner: Color) {
        self.nodes[0].visits += 1;
        for k in 0..self.path.len() {
            let (ni, ei, _) = self.path[k];
            let mover = self.nodes[ni as usize].to_move;
            let child = self.nodes[ni as usize].edges[ei as usize].child;
            if child != NONE {
                let c = &mut self.nodes[child as usize];
                c.visits += 1;
                if winner == mover {
                    c.wins += 1;
                }
            }
        }

        // RAVE: a move is credited at every node above the point where it was
        // first played, which is the whole idea — "this move, some time soon".
        let mut bits = [Bits::new(), Bits::new()];
        let mut filled_from = self.seq.len();
        for k in (0..self.path.len()).rev() {
            let (ni, _, pre_len) = self.path[k];
            let from = pre_len as usize;
            for &(pt, colour) in &self.seq[from..filled_from] {
                bits[colour.index()].set(pt as usize);
            }
            filled_from = from;
            let mover = self.nodes[ni as usize].to_move;
            let mine = &bits[mover.index()];
            let won = winner == mover;
            for e in self.nodes[ni as usize].edges.iter_mut() {
                if let Move::Play(pt) = e.mv
                    && mine.get(pt as usize)
                {
                    e.rave_visits += 1;
                    if won {
                        e.rave_wins += 1;
                    }
                }
            }
        }
    }

    fn principal_variation(&self, board: &Board, first: Move) -> Vec<Move> {
        let mut pv = vec![first];
        let Some(e0) = self.nodes[0].edges.iter().find(|e| e.mv == first) else {
            return pv;
        };
        let mut node = e0.child;
        let mut probe = *board;
        let _ = probe.play(first);
        while node != NONE && pv.len() < 8 {
            let n = &self.nodes[node as usize];
            let best = n
                .edges
                .iter()
                .filter(|e| e.child != NONE)
                .max_by_key(|e| self.nodes[e.child as usize].visits);
            let Some(e) = best else { break };
            if self.nodes[e.child as usize].visits < 8 {
                break;
            }
            if probe.play(e.mv).is_err() {
                break;
            }
            pv.push(e.mv);
            node = e.child;
        }
        pv
    }
}

/// A light playout: answer an atari next to the last move if there is one,
/// otherwise a random legal move that neither fills an own eye nor puts the new
/// chain straight into atari. Those two filters are what make random Go
/// terminate and what keep the estimate from being pure noise.
///
/// The empty points are carried in a list rather than found by scanning the
/// board each move. Scanning was what made this slow: 81 full legality checks
/// per move, against the two or three a sample-and-test needs.
fn playout(
    board: &mut Board,
    rng: &mut Rng,
    seq: &mut Vec<(Point, Color)>,
    empties: &mut Vec<Point>,
    params: &Params,
) -> Color {
    empties.clear();
    empties.extend(board.points().filter(|&pt| board.cell(pt) == Cell::Empty));
    let limit = board.size() * board.size() * 3 + 40;
    let mut played = 0;
    while board.passes() < 2 && played < limit {
        let colour = board.to_move();
        let mut from_list: Option<usize> = None;
        let mut choice = None;
        if rng.chance(params.atari_answer_percent) {
            choice = if played == 0 {
                // The position the playout starts from was handed to us, so an
                // atari in it was not created by the last move and the local
                // scan below would walk straight past it.
                atari_reply(board, colour)
            } else {
                match board.last_move() {
                    Some(Move::Play(last)) => atari_answer(board, last, colour, rng),
                    _ => None,
                }
            };
        }
        if choice.is_none() && !empties.is_empty() {
            let n = empties.len();
            // A few uniform samples first, so the usual case stays unbiased,
            // then an ordered walk so that a nearly full board still finds the
            // one playable point instead of giving up and passing.
            let mut found = None;
            for _ in 0..8 {
                let idx = rng.below(n);
                if playable(board, empties[idx], colour) {
                    found = Some(idx);
                    break;
                }
            }
            if found.is_none() {
                let start = rng.below(n);
                for k in 0..n {
                    let idx = if start + k < n {
                        start + k
                    } else {
                        start + k - n
                    };
                    if playable(board, empties[idx], colour) {
                        found = Some(idx);
                        break;
                    }
                }
            }
            if let Some(idx) = found {
                from_list = Some(idx);
                choice = Some(Move::Play(empties[idx]));
            }
        }
        let mv = choice.unwrap_or(Move::Pass);
        let taken_before = board.prisoners(Color::Black) + board.prisoners(Color::White);
        if board.play(mv).is_err() {
            let _ = board.play(Move::Pass);
        } else {
            if let Move::Play(pt) = mv {
                seq.push((pt, colour));
                if board.prisoners(Color::Black) + board.prisoners(Color::White) != taken_before {
                    // A capture re-opens points, so the list has to be rebuilt.
                    empties.clear();
                    empties.extend(board.points().filter(|&p| board.cell(p) == Cell::Empty));
                } else {
                    match from_list {
                        Some(idx) if empties[idx] == pt => {
                            empties.swap_remove(idx);
                        }
                        _ => {
                            if let Some(i) = empties.iter().position(|&q| q == pt) {
                                empties.swap_remove(i);
                            }
                        }
                    }
                }
            }
        }
        played += 1;
    }
    if board.score_playout(params.komi) > 0.0 {
        Color::Black
    } else {
        Color::White
    }
}

#[inline]
fn playable(board: &Board, pt: Point, colour: Color) -> bool {
    board.cell(pt) == Cell::Empty
        && !board.is_eyelike(pt, colour)
        && board.is_legal(pt, colour)
        && !board.is_self_atari(pt, colour)
}

/// Answers the most valuable atari anywhere on the board: captures the largest
/// enemy chain that has one liberty left, or saves the largest of one's own.
/// Public because it is worth being able to test on its own, and because it is
/// a useful thing to ask of a position in its own right.
///
/// This runs once per playout, on its first move. It is what lets the search
/// read a position it did not play itself — a handicap setup, or a game joined
/// halfway — where a chain has been sitting in atari for some moves and the
/// local heuristic has nothing to trigger on.
pub fn atari_reply(board: &Board, colour: Color) -> Option<Move> {
    let mut best: Option<(u32, Point)> = None;
    for pt in board.points() {
        let cell = board.cell(pt);
        if cell == Cell::Empty {
            continue;
        }
        if board.chain_liberties(pt, 2) != 1 {
            continue;
        }
        let Some(lib) = board.chain_single_liberty(pt) else {
            continue;
        };
        if !board.is_legal(lib, colour) {
            continue;
        }
        let mine = cell == colour.cell();
        // Running from an atari into another atari is not an answer.
        if mine && board.is_self_atari(lib, colour) {
            continue;
        }
        // Stones at stake decide, and taking beats saving the same number
        // because it also ends the question.
        let stake = board.chain_size(pt) * 2 + u32::from(!mine);
        if best.is_none_or(|(s, _)| stake > s) {
            best = Some((stake, lib));
        }
    }
    best.map(|(_, lib)| Move::Play(lib))
}

/// Save a chain of `colour` that the stone at `last` just put in atari, or
/// capture the chain that did it. Picks among the options at random.
fn atari_answer(board: &Board, last: Point, colour: Color, rng: &mut Rng) -> Option<Move> {
    let mut options: [Point; 8] = [0; 8];
    let mut n = 0;
    let mine = colour.cell();
    let theirs = colour.other().cell();
    // The attacking stone's own chain may be capturable.
    if board.cell(last) == theirs
        && board.chain_liberties(last, 2) == 1
        && let Some(lib) = board.chain_single_liberty(last)
        && board.is_legal(lib, colour)
        && n < 8
    {
        options[n] = lib;
        n += 1;
    }
    for d in [1u16, 2, 3, 4] {
        let nb = match d {
            1 => last.wrapping_sub(1),
            2 => last.wrapping_add(1),
            3 => last.wrapping_sub(crate::board::STRIDE as u16),
            _ => last.wrapping_add(crate::board::STRIDE as u16),
        };
        if !board.is_on_board(nb) {
            continue;
        }
        let c = board.cell(nb);
        if c == mine && board.chain_liberties(nb, 2) == 1 {
            if let Some(lib) = board.chain_single_liberty(nb) {
                // Running only helps if the escape square is not itself atari.
                if board.is_legal(lib, colour) && !board.is_self_atari(lib, colour) && n < 8 {
                    options[n] = lib;
                    n += 1;
                }
            }
        } else if c == theirs
            && board.chain_liberties(nb, 2) == 1
            && let Some(lib) = board.chain_single_liberty(nb)
            && board.is_legal(lib, colour)
            && n < 8
        {
            options[n] = lib;
            n += 1;
        }
    }
    if n == 0 {
        None
    } else {
        Some(Move::Play(options[rng.below(n)]))
    }
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub mv: Move,
    pub visits: u64,
    pub wins: u64,
    pub pv: Vec<Move>,
}

impl Candidate {
    pub fn winrate(&self) -> f64 {
        if self.visits == 0 {
            0.0
        } else {
            self.wins as f64 / self.visits as f64
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    /// The move with the most visits summed over every tree.
    pub best: Move,
    pub candidates: Vec<Candidate>,
    pub playouts: u64,
    pub elapsed: Duration,
    pub threads: usize,
}

impl SearchResult {
    pub fn winrate(&self) -> f64 {
        self.candidates.first().map_or(0.0, Candidate::winrate)
    }
}

/// A root move and the line the search expects to follow it.
type RootLine = (Move, Vec<Move>);

/// Searches `board` for the side to move and returns the moves worth playing,
/// most-visited first.
pub fn search(board: &Board, params: &Params) -> SearchResult {
    let start = Instant::now();
    let threads = params.threads.max(1);
    let per_thread_playouts = params.playouts.map(|p| p.div_ceil(threads as u64));

    let mut summaries: Vec<Vec<(Move, u64, u64)>> = Vec::new();
    let mut pvs: Vec<(u64, Vec<RootLine>)> = Vec::new();
    let mut total_playouts = 0u64;

    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for t in 0..threads {
            let params = *params;
            let board = *board;
            handles.push(scope.spawn(move || {
                let mut tree = Tree::new(
                    &board,
                    params,
                    params.seed ^ (t as u64).wrapping_mul(0x9E37_79B9),
                );
                let deadline = params.time_limit.map(|d| Instant::now() + d);
                loop {
                    if let Some(n) = per_thread_playouts
                        && tree.playouts >= n
                    {
                        break;
                    }
                    if let Some(dl) = deadline {
                        // Checking the clock every playout costs less than a
                        // playout; checking it every 64 would overshoot.
                        if Instant::now() >= dl {
                            break;
                        }
                    }
                    if per_thread_playouts.is_none() && deadline.is_none() {
                        break;
                    }
                    tree.iterate(&board);
                }
                let root_stats: Vec<(Move, u64, u64)> = tree.nodes[0]
                    .edges
                    .iter()
                    .map(|e| {
                        if e.child == NONE {
                            (e.mv, 0, 0)
                        } else {
                            let c = &tree.nodes[e.child as usize];
                            (e.mv, c.visits as u64, c.wins as u64)
                        }
                    })
                    .collect();
                let pv_for: Vec<RootLine> = root_stats
                    .iter()
                    .filter(|(_, v, _)| *v > 0)
                    .map(|(mv, _, _)| (*mv, tree.principal_variation(&board, *mv)))
                    .collect();
                (root_stats, tree.playouts, pv_for)
            }));
        }
        for h in handles {
            let (stats, playouts, pv_for) = h.join().expect("search thread panicked");
            total_playouts += playouts;
            summaries.push(stats);
            pvs.push((playouts, pv_for));
        }
    });

    let mut merged: Vec<(Move, u64, u64)> = Vec::new();
    for stats in &summaries {
        for &(mv, v, w) in stats {
            match merged.iter_mut().find(|(m, _, _)| *m == mv) {
                Some(entry) => {
                    entry.1 += v;
                    entry.2 += w;
                }
                None => merged.push((mv, v, w)),
            }
        }
    }
    merged.sort_by_key(|&(_, visits, _)| std::cmp::Reverse(visits));

    // The principal variations come from the tree that searched deepest; mixing
    // lines from several trees would read like a line nobody actually searched.
    let deepest = pvs
        .iter()
        .max_by_key(|(p, _)| *p)
        .map(|(_, pv)| pv.clone())
        .unwrap_or_default();

    let candidates: Vec<Candidate> = merged
        .iter()
        .map(|&(mv, visits, wins)| Candidate {
            mv,
            visits,
            wins,
            pv: deepest
                .iter()
                .find(|(m, _)| *m == mv)
                .map(|(_, pv)| pv.clone())
                .unwrap_or_else(|| vec![mv]),
        })
        .collect();

    let best = candidates.first().map_or(Move::Pass, |c| c.mv);
    SearchResult {
        best,
        candidates,
        playouts: total_playouts,
        elapsed: start.elapsed(),
        threads,
    }
}
