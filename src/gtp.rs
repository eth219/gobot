//! Go Text Protocol, enough of it to be driven by `gogui-twogtp`.
//!
//! This is here to answer one question the terminal board cannot: is a change to
//! the search actually stronger? Two builds play a few hundred games against
//! each other and the win rate says so.

use std::io::{BufRead, Write};

use crate::board::{Board, Color, Move};
use crate::coords::{format_move, parse_move};
use crate::game::Game;
use crate::mcts::{self, Params};

const COMMANDS: &[&str] = &[
    "protocol_version",
    "name",
    "version",
    "known_command",
    "list_commands",
    "quit",
    "boardsize",
    "clear_board",
    "komi",
    "play",
    "genmove",
    "showboard",
    "final_score",
    "time_settings",
    "undo",
    "fixed_handicap",
];

pub fn run(size: usize, params: Params) -> std::io::Result<()> {
    let mut params = params;
    let mut game = Game::new(size, params.komi);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.split('#').next().unwrap_or("").trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        // An optional leading integer is the command id, echoed back.
        let first = parts.next().unwrap_or("");
        let (id, cmd) = match first.parse::<u32>() {
            Ok(n) => (Some(n), parts.next().unwrap_or("").to_string()),
            Err(_) => (None, first.to_string()),
        };
        let args: Vec<String> = parts.map(str::to_string).collect();

        let response = match cmd.as_str() {
            "protocol_version" => Ok("2".to_string()),
            "name" => Ok("gobot".to_string()),
            "version" => Ok(env!("CARGO_PKG_VERSION").to_string()),
            "list_commands" => Ok(COMMANDS.join("\n")),
            "known_command" => Ok(COMMANDS.contains(&args[0].as_str()).to_string()),
            "quit" => {
                write_response(&mut out, id, Ok("".to_string()))?;
                return Ok(());
            }
            "boardsize" => match args.first().and_then(|a| a.parse::<usize>().ok()) {
                Some(n) if (2..=crate::board::MAX_SIZE).contains(&n) => {
                    game = Game::new(n, params.komi);
                    Ok(String::new())
                }
                _ => Err("unacceptable size".to_string()),
            },
            "clear_board" => {
                let n = game.board().size();
                game = Game::new(n, params.komi);
                Ok(String::new())
            }
            "komi" => match args.first().and_then(|a| a.parse::<f32>().ok()) {
                Some(k) => {
                    params.komi = k;
                    game.komi = k;
                    Ok(String::new())
                }
                None => Err("komi not a float".to_string()),
            },
            "play" => play_command(&mut game, &args),
            "genmove" => genmove_command(&mut game, &args, &params),
            "showboard" => Ok(format!("\n{}", crate::coords::diagram(game.board(), &[]))),
            "undo" => {
                if game.undo() {
                    Ok(String::new())
                } else {
                    Err("cannot undo".to_string())
                }
            }
            "fixed_handicap" => fixed_handicap(&mut game, &args),
            "final_score" => Ok(format_score(game.score())),
            "time_settings" => Ok(String::new()),
            other => Err(format!("unknown command: {other}")),
        };
        write_response(&mut out, id, response)?;
    }
    Ok(())
}

fn parse_colour(s: &str) -> Option<Color> {
    match s.to_ascii_lowercase().as_str() {
        "b" | "black" => Some(Color::Black),
        "w" | "white" => Some(Color::White),
        _ => None,
    }
}

fn play_command(game: &mut Game, args: &[String]) -> Result<String, String> {
    if args.len() < 2 {
        return Err("syntax error".to_string());
    }
    let colour = parse_colour(&args[0]).ok_or("invalid colour")?;
    let mv = parse_move(game.board(), &args[1]).ok_or("invalid coordinate")?;
    game.set_to_move(colour);
    game.play(mv)
        .map_err(|why| format!("illegal move: {why:?}"))?;
    Ok(String::new())
}

fn genmove_command(game: &mut Game, args: &[String], params: &Params) -> Result<String, String> {
    let colour = args
        .first()
        .and_then(|a| parse_colour(a))
        .ok_or("invalid colour")?;
    game.set_to_move(colour);
    let result = mcts::search(game.board(), params);
    // Below one playout in ten there is no position left worth playing out.
    let mv = if result.winrate() < 0.10 && result.playouts > 1000 {
        Move::Resign
    } else {
        result.best
    };
    if let Move::Play(_) | Move::Pass = mv {
        let _ = game.play(mv);
    }
    Ok(format_move(game.board(), mv))
}

/// Lays out the conventional handicap stones and reports where they went. The
/// caller is told rather than assumed to agree, which is what keeps two engines
/// on the same board.
fn fixed_handicap(game: &mut Game, args: &[String]) -> Result<String, String> {
    let n = args
        .first()
        .and_then(|a| a.parse::<usize>().ok())
        .ok_or("syntax error")?;
    let size = game.board().size();
    let points = crate::coords::handicap_points(size, n)?;
    let mut board = Board::new(size);
    let mut names = Vec::new();
    for (x, y) in points {
        let pt = board.point(x, y);
        board.setup_stone(pt, Color::Black);
        names.push(format_move(&board, Move::Play(pt)));
    }
    board.set_to_move(Color::White);
    let komi = game.komi;
    *game = Game::from_board(board, komi);
    Ok(names.join(" "))
}

fn format_score(score: f32) -> String {
    if score > 0.0 {
        format!("B+{score:.1}")
    } else if score < 0.0 {
        format!("W+{:.1}", -score)
    } else {
        "0".to_string()
    }
}

fn write_response(
    out: &mut impl Write,
    id: Option<u32>,
    response: Result<String, String>,
) -> std::io::Result<()> {
    let (sign, body) = match response {
        Ok(body) => ('=', body),
        Err(body) => ('?', body),
    };
    match id {
        Some(n) => write!(out, "{sign}{n} {body}\n\n")?,
        None => write!(out, "{sign} {body}\n\n")?,
    }
    out.flush()
}
