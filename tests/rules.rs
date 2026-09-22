//! The rules. Every test here is written so that it would fail if the rule it
//! names were simply absent — a suite that only checks legal moves stay legal
//! passes just as well on an engine with no rules at all.

use gobot::board::{Board, Cell, Color, Illegal, Move};
use gobot::coords::{diagram, parse_diagram, parse_diagram_report, parse_move};
use gobot::game::{Game, Rejected};

fn at(b: &Board, v: &str) -> gobot::board::Point {
    match parse_move(b, v).expect("vertex") {
        Move::Play(pt) => pt,
        _ => panic!("not a point"),
    }
}

#[test]
fn a_surrounded_stone_is_captured() {
    let mut b = Board::new(9);
    b.setup_stone(at(&b, "e5"), Color::White);
    for v in ["e6", "e4", "d5"] {
        b.setup_stone(at(&b, v), Color::Black);
    }
    assert_eq!(b.cell(at(&b, "e5")), Cell::White, "still one liberty at f5");
    b.set_to_move(Color::Black);
    b.play(Move::Play(at(&b, "f5"))).expect("legal");
    assert_eq!(
        b.cell(at(&b, "e5")),
        Cell::Empty,
        "the stone should be gone"
    );
    assert_eq!(b.prisoners(Color::Black), 1);
}

#[test]
fn a_whole_chain_goes_at_once() {
    // A white wall down the a file, black alongside it, one liberty left at a4.
    let mut b = Board::new(9);
    for v in ["a1", "a2", "a3"] {
        b.setup_stone(at(&b, v), Color::White);
    }
    for v in ["b1", "b2", "b3"] {
        b.setup_stone(at(&b, v), Color::Black);
    }
    assert_eq!(b.chain_liberties(at(&b, "a1"), 9), 1);
    assert_eq!(b.chain_single_liberty(at(&b, "a1")), Some(at(&b, "a4")));
    b.set_to_move(Color::Black);
    b.play(Move::Play(at(&b, "a4"))).expect("legal");
    for v in ["a1", "a2", "a3"] {
        assert_eq!(b.cell(at(&b, v)), Cell::Empty, "{v} should be captured");
    }
    assert_eq!(b.prisoners(Color::Black), 3);
}

#[test]
fn filling_your_own_last_liberty_is_suicide() {
    let mut b = Board::new(9);
    for v in ["e6", "e4", "d5", "f5"] {
        b.setup_stone(at(&b, v), Color::Black);
    }
    assert_eq!(
        b.illegal_reason(at(&b, "e5"), Color::White),
        Some(Illegal::Suicide)
    );
    // The same point is legal for the colour that owns the surround.
    assert_eq!(b.illegal_reason(at(&b, "e5"), Color::Black), None);
}

#[test]
fn a_move_that_captures_is_not_suicide() {
    // White fills its own last liberty, but the black chain dies first.
    let mut b = Board::new(9);
    for v in ["a1", "a2"] {
        b.setup_stone(at(&b, v), Color::Black);
    }
    for v in ["b1", "b2", "a3"] {
        b.setup_stone(at(&b, v), Color::White);
    }
    // Black a1/a2 now has no liberties at all, so rebuild it the legal way.
    let mut b = Board::new(9);
    b.setup_stone(at(&b, "a1"), Color::Black);
    b.setup_stone(at(&b, "b1"), Color::White);
    b.setup_stone(at(&b, "b2"), Color::White);
    // Black a1's only liberty is a2; white a2 captures it and would otherwise
    // be a stone with no liberty of its own.
    assert_eq!(b.chain_single_liberty(at(&b, "a1")), Some(at(&b, "a2")));
    assert_eq!(b.illegal_reason(at(&b, "a2"), Color::White), None);
    b.set_to_move(Color::White);
    b.play(Move::Play(at(&b, "a2"))).expect("capture is legal");
    assert_eq!(b.cell(at(&b, "a1")), Cell::Empty);
}

/// The ko shape, laid out once and shared by the two tests below.
///
/// ```text
///      d  e  f  g
///   6  .  X  O  .
///   5  X  O  .  O
///   4  .  X  O  .
/// ```
///
/// White e5 is a lone stone in atari; Black f5 takes it and is itself then a
/// lone stone with one liberty, which is exactly the ko condition.
fn ko_position() -> Board {
    let mut b = Board::new(9);
    for v in ["e6", "d5", "e4"] {
        let pt = at(&b, v);
        b.setup_stone(pt, Color::Black);
    }
    for v in ["f6", "e5", "g5", "f4"] {
        let pt = at(&b, v);
        b.setup_stone(pt, Color::White);
    }
    b.set_to_move(Color::Black);
    b
}

#[test]
fn ko_forbids_the_immediate_retake_and_only_that() {
    let mut b = ko_position();
    assert_eq!(
        b.chain_liberties(at(&b, "e5"), 9),
        1,
        "white e5 should be in atari"
    );
    b.play(Move::Play(at(&b, "f5"))).expect("legal capture");
    assert_eq!(b.cell(at(&b, "e5")), Cell::Empty);
    assert_eq!(b.prisoners(Color::Black), 1);
    assert_eq!(b.ko_point(), Some(at(&b, "e5")), "e5 is now the ko point");
    assert_eq!(
        b.illegal_reason(at(&b, "e5"), Color::White),
        Some(Illegal::Ko),
        "white must not take straight back"
    );
    // A move elsewhere clears the ko, and then the retake is a rules-legal move
    // again — which is the hole that superko exists to close.
    b.play(Move::Play(at(&b, "a1"))).expect("legal");
    assert_eq!(b.ko_point(), None);
    assert_eq!(b.illegal_reason(at(&b, "e5"), Color::White), None);
}

#[test]
fn superko_catches_a_repeat_that_simple_ko_misses() {
    // Simple ko looks one move back, so a pass on each side clears it while
    // leaving the stones untouched. White's retake then restores a position the
    // game has already stood in, and only the history can see that.
    let board = ko_position();
    let opening = board.hash();
    let mut g = Game::from_board(board, 7.5);

    g.play(Move::Play(at(g.board(), "f5")))
        .expect("black takes");
    g.play(Move::Pass).expect("white passes");
    g.play(Move::Pass).expect("black passes");
    assert_eq!(
        g.board().ko_point(),
        None,
        "the passes cleared the simple ko flag"
    );

    let retake = Move::Play(at(g.board(), "e5"));
    let mut bare = *g.board();
    bare.play(retake).expect("the bare rules allow the retake");
    assert_eq!(bare.hash(), opening, "and it restores the opening position");

    assert_eq!(
        g.play(retake),
        Err(Rejected::Superko),
        "the game must refuse a position it has already stood in"
    );
}

#[test]
fn an_eye_is_not_a_place_to_play_but_a_false_eye_is() {
    let mut b = Board::new(9);
    // A one-point eye at a1, enclosed by black.
    for v in ["a2", "b1", "b2"] {
        b.setup_stone(at(&b, v), Color::Black);
    }
    assert!(b.is_eyelike(at(&b, "a1"), Color::Black));
    // A white stone on the b2 diagonal at the edge makes it false.
    b.remove_stone(at(&b, "b2"));
    b.setup_stone(at(&b, "b2"), Color::White);
    assert!(
        !b.is_eyelike(at(&b, "a1"), Color::Black),
        "a hostile diagonal at the edge is one too many"
    );
}

#[test]
fn area_scoring_counts_enclosed_empty_points() {
    // Black owns the whole 5x5 board bar a white corner group.
    let text = "\
. . . . .
. . . . .
X X X X X
O O O O O
O O O O O
";
    let b = parse_diagram(text, 5).expect("diagram");
    // Top two rows are empty and touch only black, so they are black's.
    assert_eq!(b.score_area(0.0), 10.0 + 5.0 - 10.0);
    // With komi the same position is white's.
    assert_eq!(b.score_area(7.5), 10.0 + 5.0 - 10.0 - 7.5);
}

#[test]
fn a_dame_between_two_colours_belongs_to_neither() {
    let text = "\
X . O
X . O
X . O
";
    let b = parse_diagram(text, 3).expect("diagram");
    assert_eq!(b.score_area(0.0), 0.0, "the middle file touches both");
}

#[test]
fn a_diagram_survives_a_round_trip() {
    let text = "\
. . . . . . . . .
. . . . . . . . .
. . X . . . O . .
. . . . . . . . .
. . . . X . . . .
. . . . . . . . .
. . O . . . X . .
. . . . . . . . .
. . . . . . . . .
";
    let b = parse_diagram(text, 9).expect("diagram");
    let printed = diagram(&b, &[]);
    let b2 = parse_diagram(&printed, 9).expect("reparse");
    assert_eq!(
        b.hash(),
        b2.hash(),
        "printing then reading should not move a stone"
    );
}

#[test]
fn a_diagram_of_the_wrong_shape_is_refused() {
    assert!(
        parse_diagram(". . .\n. . .\n", 3).is_err(),
        "two rows, not three"
    );
    assert!(
        parse_diagram(". . .\n. .\n. . .\n", 3).is_err(),
        "a short row"
    );
}

#[test]
fn vertex_names_skip_the_letter_i() {
    let b = Board::new(9);
    assert_eq!(b.coords(at(&b, "a1")), (0, 0));
    assert_eq!(
        b.coords(at(&b, "j9")),
        (8, 8),
        "j is the ninth column, not i"
    );
    assert!(parse_move(&b, "i5").is_none(), "there is no column i");
    assert!(parse_move(&b, "k1").is_none(), "off a 9x9 board");
    assert_eq!(parse_move(&b, "pass"), Some(Move::Pass));
}

#[test]
fn a_position_survives_being_replayed_point_by_point() {
    // How a position is handed to another engine: every stone is sent as a
    // `play` with its colour named, in row-major order, because a position
    // typed in by hand has no move history to replay instead.
    //
    // That is only sound if no intermediate step captures something the final
    // position keeps. It holds because the board is always a subset of the
    // final one, so a finished chain has at least its final liberties and an
    // unfinished one has the empty points its own missing stones will fill.
    // This test is what would notice if that reasoning were wrong.
    let mut rng = gobot::mcts::Rng::new(0xD1A6);
    for round in 0..40 {
        let mut original = Board::new(9);
        for _ in 0..(20 + round * 2) {
            let colour = original.to_move();
            let mut legal: Vec<Move> = Vec::new();
            for pt in original.points() {
                if original.cell(pt) == Cell::Empty
                    && !original.is_eyelike(pt, colour)
                    && original.is_legal(pt, colour)
                {
                    legal.push(Move::Play(pt));
                }
            }
            if legal.is_empty() {
                break;
            }
            let _ = original.play(legal[rng.below(legal.len())]);
        }

        let mut replayed = Board::new(9);
        for pt in original.points() {
            let colour = match original.cell(pt) {
                Cell::Black => Color::Black,
                Cell::White => Color::White,
                _ => continue,
            };
            replayed.set_to_move(colour);
            assert!(
                replayed.play(Move::Play(pt)).is_ok(),
                "round {round}: replaying the position reached an illegal move"
            );
        }
        assert_eq!(
            replayed.hash(),
            original.hash(),
            "round {round}: the replayed position is not the one we started from"
        );
    }
}

#[test]
fn a_diagram_says_which_stones_it_had_to_drop() {
    // A stone with no liberties cannot be on a board. Taking it off is right;
    // doing it silently means one mis-copied point analyses a different game.
    let text = "\
O X . . .
X . . . .
. . . . .
. . . . .
. . . . .
";
    let (board, dropped) = parse_diagram_report(text, 5).expect("diagram");
    assert_eq!(dropped.len(), 1, "the white stone cannot be there");
    assert_eq!(dropped[0], at(&board, "a5"));
    assert_eq!(board.cell(at(&board, "a5")), Cell::Empty);
    assert_eq!(board.cell(at(&board, "b5")), Cell::Black, "the rest stands");

    let (_, dropped) = parse_diagram_report(". . .\n. X .\n. . .\n", 3).expect("diagram");
    assert!(dropped.is_empty(), "a lone stone is fine");
}

#[test]
fn handicap_points_are_the_conventional_ones() {
    use gobot::coords::handicap_points;
    // 9x9 stars sit two from the edge; 19x19 three.
    assert_eq!(handicap_points(9, 2).unwrap(), vec![(2, 2), (6, 6)]);
    assert_eq!(handicap_points(19, 2).unwrap(), vec![(3, 3), (15, 15)]);
    assert_eq!(
        handicap_points(9, 4).unwrap(),
        vec![(2, 2), (6, 6), (2, 6), (6, 2)],
        "four corners before any side point"
    );
    assert_eq!(
        handicap_points(9, 5).unwrap().last(),
        Some(&(4, 4)),
        "the fifth stone is the centre"
    );
    assert_eq!(handicap_points(9, 9).unwrap().len(), 9);
    assert!(
        handicap_points(9, 1).is_err(),
        "one stone is not a handicap"
    );
    assert!(handicap_points(9, 10).is_err());
    assert!(handicap_points(5, 2).is_err(), "no star points that small");
    assert!(
        handicap_points(8, 5).is_err(),
        "an even board has no centre point"
    );
    assert!(handicap_points(8, 4).is_ok(), "but it has four corners");
}
