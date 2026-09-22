//! The search. Each test here is arranged so that a broken search fails it: a
//! suite that only checks the engine returns *a* legal move would pass on an
//! engine that answers at random, which is the thing being ruled out.

use gobot::board::{Board, Cell, Color, Move, Point};
use gobot::coords::{format_move, parse_move};
use gobot::game::Game;
use gobot::mcts::{self, Params, Rng};

fn fixed(playouts: u64, seed: u64) -> Params {
    Params {
        playouts: Some(playouts),
        time_limit: None,
        threads: 4,
        seed,
        ..Params::default()
    }
}

fn at(b: &Board, v: &str) -> Point {
    match parse_move(b, v).expect("vertex") {
        Move::Play(pt) => pt,
        _ => panic!("not a point"),
    }
}

/// Black's seven stones on the b file have one liberty left at b9. Playing it
/// is the only way not to lose them, and losing seven stones on a 9x9 board
/// loses the game, so an engine that searches at all must find it.
fn seven_stones_in_atari() -> Board {
    let mut b = Board::new(9);
    for y in 2..=8 {
        let pt = b.point(1, y - 1);
        b.setup_stone(pt, Color::Black);
    }
    for y in 2..=8 {
        for x in [0usize, 2] {
            let pt = b.point(x, y - 1);
            b.setup_stone(pt, Color::White);
        }
    }
    b.setup_stone(at(&b, "b1"), Color::White);
    b.set_to_move(Color::Black);
    b
}

#[test]
fn the_position_under_the_atari_test_is_the_one_intended() {
    let b = seven_stones_in_atari();
    assert_eq!(
        b.chain_liberties(at(&b, "b5"), 9),
        1,
        "black's b file should be in atari"
    );
    assert_eq!(b.chain_size(at(&b, "b5")), 7);
    assert_eq!(b.chain_single_liberty(at(&b, "b5")), Some(at(&b, "b9")));
}

#[test]
fn the_atari_reply_takes_the_biggest_chain_going() {
    // Two chains in atari at once: a white pair the engine can capture and a
    // black seven it can save. Saving seven beats taking two.
    let b = seven_stones_in_atari();
    let mut b = b;
    b.setup_stone(at(&b, "h1"), Color::White);
    b.setup_stone(at(&b, "h2"), Color::Black);
    b.setup_stone(at(&b, "g1"), Color::Black);
    assert_eq!(
        b.chain_liberties(at(&b, "h1"), 9),
        1,
        "the white stone should be in atari at j1"
    );
    let reply = mcts::atari_reply(&b, Color::Black).expect("something is in atari");
    assert_eq!(
        format_move(&b, reply),
        "B9",
        "seven of its own stones outweigh one of the opponent's"
    );
}

#[test]
fn the_atari_reply_sees_an_atari_nobody_just_played() {
    // The whole point: this position was handed to the engine, so the atari in
    // it was not created by the last move. A reply policy that only looked at
    // the last move — which is what this engine did at first — sees nothing
    // here, and the test would fail with `None`.
    let b = seven_stones_in_atari();
    assert_eq!(b.last_move(), None, "nothing was played to reach this");
    let reply = mcts::atari_reply(&b, Color::Black).expect("black's b file is in atari");
    assert_eq!(format_move(&b, reply), "B9");
    // And from the other side the same point captures instead of saving.
    let mut white = b;
    white.set_to_move(Color::White);
    let reply = mcts::atari_reply(&white, Color::White).expect("seven stones to take");
    assert_eq!(format_move(&white, reply), "B9");
}

#[test]
fn the_atari_reply_stays_quiet_when_nothing_is_in_atari() {
    let b = Board::new(9);
    assert!(
        mcts::atari_reply(&b, Color::Black).is_none(),
        "an empty board"
    );
    let mut b = Board::new(9);
    b.setup_stone(at(&b, "e5"), Color::White);
    b.setup_stone(at(&b, "e6"), Color::Black);
    assert_eq!(b.chain_liberties(at(&b, "e5"), 9), 3);
    assert!(
        mcts::atari_reply(&b, Color::Black).is_none(),
        "three liberties is not an atari"
    );
}

#[test]
fn it_does_not_spend_a_move_taking_stones_that_are_already_dead() {
    // White's two stones have one liberty and nothing to run to, so they are
    // dead where they stand. Capturing them right now gains nothing that
    // waiting does not, and a move on the open board is worth more — so the
    // capture should *not* be the engine's first choice.
    let b =
        gobot::coords::parse_diagram("O O . . .\nX X X . .\n. . . . .\n. . . . .\n. . . . .\n", 5)
            .expect("diagram");
    let mut b = b;
    b.set_to_move(Color::Black);
    assert_eq!(
        mcts::atari_reply(&b, Color::Black).map(|m| format_move(&b, m)),
        Some("C5".to_string()),
        "the capture is available"
    );
    let result = mcts::search(&b, &fixed(60_000, 0xC0FFEE));
    assert_ne!(
        format_move(&b, result.best),
        "C5",
        "it should be playing on the open board, not collecting dead stones"
    );
    assert!(
        result.winrate() > 0.7,
        "black is clearly ahead here, engine says {:.2}",
        result.winrate()
    );
}

/// A uniform random player, filtered only so that it does not fill its own eyes
/// — without that filter a random game does not end.
fn random_move(board: &Board, rng: &mut Rng) -> Move {
    let mut legal: Vec<Move> = Vec::new();
    let colour = board.to_move();
    for pt in board.points() {
        if board.cell(pt) == Cell::Empty
            && !board.is_eyelike(pt, colour)
            && board.is_legal(pt, colour)
        {
            legal.push(Move::Play(pt));
        }
    }
    if legal.is_empty() {
        Move::Pass
    } else {
        legal[rng.below(legal.len())]
    }
}

enum Player {
    Engine(Params),
    Random,
}

/// Plays one game out and returns the area score, positive for Black.
fn play_game(black: &Player, white: &Player, komi: f32, seed: u64) -> f32 {
    let mut game = Game::new(9, komi);
    let mut rng = Rng::new(seed);
    for _ in 0..400 {
        if game.board().passes() >= 2 {
            break;
        }
        let player = match game.to_move() {
            Color::Black => black,
            Color::White => white,
        };
        let mv = match player {
            Player::Engine(p) => {
                let mut p = *p;
                p.komi = komi;
                p.seed ^= game.moves_played() as u64;
                mcts::search(game.board(), &p).best
            }
            Player::Random => random_move(game.board(), &mut rng),
        };
        if game.play(mv).is_err() {
            // Superko or a stale move: pass rather than stall the game.
            let _ = game.play(Move::Pass);
        }
    }
    game.score()
}

#[test]
fn it_beats_a_random_player_every_time() {
    let mut wins = 0;
    let games = 6;
    for i in 0..games {
        let score = play_game(
            &Player::Engine(fixed(1_500, 0x5EED ^ i)),
            &Player::Random,
            7.5,
            0xBEEF ^ i,
        );
        if score > 0.0 {
            wins += 1;
        }
    }
    assert_eq!(
        wins, games,
        "1500 playouts a move should not lose a 9x9 game to random play"
    );
}

#[test]
fn more_playouts_beat_fewer() {
    // The point of this one is the search itself. A tree that gathered its
    // statistics the wrong way round, or backed them up with the sign flipped,
    // would get *worse* with more playouts, and this is the test that notices.
    let strong = 4_000;
    let weak = 150;
    let mut strong_wins = 0;
    let games = 4;
    for i in 0..games {
        // Alternate colours so that komi does not decide it.
        let score = if i % 2 == 0 {
            play_game(
                &Player::Engine(fixed(strong, 0x11 ^ i)),
                &Player::Engine(fixed(weak, 0x22 ^ i)),
                7.5,
                i,
            )
        } else {
            -play_game(
                &Player::Engine(fixed(weak, 0x33 ^ i)),
                &Player::Engine(fixed(strong, 0x44 ^ i)),
                7.5,
                i,
            )
        };
        if score > 0.0 {
            strong_wins += 1;
        }
    }
    assert!(
        strong_wins >= 3,
        "{strong} playouts won only {strong_wins} of {games} against {weak}"
    );
}

#[test]
fn it_never_offers_an_illegal_move() {
    // Random positions, including crowded ones, and every suggestion has to be
    // one the rules would accept.
    let mut rng = Rng::new(0xF00D);
    for game in 0..8 {
        let mut board = Board::new(9);
        for _ in 0..(10 + game * 6) {
            let mv = random_move(&board, &mut rng);
            if board.play(mv).is_err() {
                break;
            }
        }
        if board.points().all(|pt| board.cell(pt) != Cell::Empty) {
            continue;
        }
        let result = mcts::search(&board, &fixed(600, 0x99 ^ game));
        for c in result.candidates.iter().filter(|c| c.visits > 0) {
            if let Move::Play(pt) = c.mv {
                assert!(
                    board.is_legal(pt, board.to_move()),
                    "suggested {} in a position where it is illegal",
                    format_move(&board, c.mv)
                );
            }
        }
    }
}

#[test]
fn it_reports_a_win_rate_that_matches_the_position() {
    // A board where Black owns everything: the engine should be confident, and
    // the same position with the colours reversed should read the other way.
    let text = "\
X X X X X X X X X
X X X X X X X X X
X X X X X X X X X
X X X X X X X X X
. . . . . . . . .
. . . . . . . . .
O . . . . . . . O
. . . . . . . . .
O . . . . . . . O
";
    let board = gobot::coords::parse_diagram(text, 9).expect("diagram");
    let mut black_to_play = board;
    black_to_play.set_to_move(Color::Black);
    let result = mcts::search(&black_to_play, &fixed(8_000, 0x1234));
    assert!(
        result.winrate() > 0.8,
        "Black is far ahead here but the engine says {:.2}",
        result.winrate()
    );

    let mut white_to_play = board;
    white_to_play.set_to_move(Color::White);
    let result = mcts::search(&white_to_play, &fixed(8_000, 0x1234));
    assert!(
        result.winrate() < 0.2,
        "White is far behind here but the engine says {:.2}",
        result.winrate()
    );
}
