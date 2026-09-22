//! A local web board.
//!
//! Typing a position in as a diagram is where the mistakes come from, so this
//! serves one page that you click stones onto. It is hand-rolled HTTP over a
//! `TcpListener` because the whole program has no dependencies and one page
//! with three endpoints does not justify breaking that.
//!
//! Requests are handled one at a time. There is one person at the board, and a
//! queue of one keeps the engine's turn honest without a mutex.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::arena::GtpEngine;
use crate::board::{Board, Cell, Color, Move};
use crate::coords::format_move;
use crate::game::{Game, Rejected};
use crate::mcts::{self, Params};

const PAGE: &str = include_str!("web/index.html");

pub struct ServeOptions {
    pub bind: String,
    pub port: u16,
    pub size: usize,
    pub komi: f32,
    pub params: Params,
    pub engine: Option<String>,
}

pub fn serve(opts: ServeOptions) -> std::io::Result<()> {
    let mut engine = match &opts.engine {
        Some(command) => match GtpEngine::spawn(command) {
            Ok(engine) => {
                println!("hints from {}", engine.label);
                Some(engine)
            }
            Err(why) => {
                eprintln!("gobot: {why}");
                std::process::exit(1);
            }
        },
        None => None,
    };

    let listener = TcpListener::bind((opts.bind.as_str(), opts.port))?;
    let shown = if opts.bind == "0.0.0.0" {
        local_addresses()
            .into_iter()
            .map(|ip| format!("http://{ip}:{}", opts.port))
            .collect::<Vec<_>>()
            .join("  ")
    } else {
        format!("http://{}:{}", opts.bind, opts.port)
    };
    println!("gobot board at {shown}");
    if opts.bind == "0.0.0.0" {
        println!("reachable from anything on this network — stop it when you are done");
    }

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        if let Err(e) = handle(&mut stream, &opts, engine.as_mut()) {
            eprintln!("gobot: {e}");
        }
    }
    Ok(())
}

fn handle(
    stream: &mut TcpStream,
    opts: &ServeOptions,
    engine: Option<&mut GtpEngine>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    }
    let body = String::from_utf8_lossy(&body).to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/") => respond(stream, "200 OK", "text/html; charset=utf-8", PAGE),
        ("GET", "/defaults") => {
            let text = format!(
                "size {}\nkomi {}\ntime {:.1}\nengine {}\n",
                opts.size,
                opts.komi,
                opts.params.time_limit.map_or(3.0, |d| d.as_secs_f64()),
                opts.engine.is_some()
            );
            respond(stream, "200 OK", "text/plain; charset=utf-8", &text)
        }
        ("POST", "/move") => {
            let text = make_move(&body, opts);
            respond(stream, "200 OK", "text/plain; charset=utf-8", &text)
        }
        ("POST", "/play") => {
            let text = play(&body, opts, engine);
            respond(stream, "200 OK", "text/plain; charset=utf-8", &text)
        }
        ("POST", "/analyse") => {
            let text = analyse(&body, opts, engine);
            respond(stream, "200 OK", "text/plain; charset=utf-8", &text)
        }
        _ => respond(
            stream,
            "404 Not Found",
            "text/plain",
            "no such thing here\n",
        ),
    }
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// A request is a few lines of `key value`, and so is an answer. There is no
/// JSON parser here for the same reason there are no dependencies.
struct Request {
    /// The position, and the history the page carried with it. Superko here is
    /// only as good as that history: the server keeps nothing between requests,
    /// so it enforces the rule for an honest page rather than against a liar.
    game: Game,
    millis: u64,
    /// A move the side to move wants to make before anything else happens.
    move_first: Option<String>,
    /// Points that were sent but that the rules would not leave on the board.
    dropped: Vec<String>,
}

impl Request {
    fn board(&self) -> &Board {
        self.game.board()
    }
}

fn parse_request(body: &str, opts: &ServeOptions) -> Result<Request, String> {
    let mut size = opts.size;
    let mut komi = opts.komi;
    let mut seconds = opts.params.time_limit.map_or(3.0, |d| d.as_secs_f64());
    let mut to_move = Color::Black;
    let mut move_first = None;
    let mut ko = None;
    let mut seen: Vec<String> = Vec::new();
    let mut stones: Vec<(usize, usize, Color)> = Vec::new();

    for line in body.lines() {
        let mut w = line.split_whitespace();
        match (w.next(), w.next()) {
            (Some("size"), Some(v)) => size = v.parse().unwrap_or(size),
            (Some("komi"), Some(v)) => komi = v.parse().unwrap_or(komi),
            (Some("time"), Some(v)) => seconds = v.parse().unwrap_or(seconds),
            (Some("move"), Some(v)) => move_first = Some(v.to_string()),
            (Some("ko"), Some(v)) => ko = Some(v.to_string()),
            (Some("seen"), Some(v)) => seen.push(v.to_string()),
            (Some("tomove"), Some(v)) => {
                to_move = if v.starts_with('w') {
                    Color::White
                } else {
                    Color::Black
                }
            }
            (Some(c @ ("b" | "w")), Some(x)) => {
                let (Ok(x), Some(Ok(y))) = (x.parse::<usize>(), w.next().map(str::parse::<usize>))
                else {
                    continue;
                };
                stones.push((x, y, if c == "b" { Color::Black } else { Color::White }));
            }
            _ => {}
        }
    }

    if !(2..=crate::board::MAX_SIZE).contains(&size) {
        return Err(format!("board size {size} is out of range"));
    }
    let mut board = Board::new(size);
    let mut wanted = Vec::new();
    for &(x, y, colour) in &stones {
        if x >= size || y >= size {
            continue;
        }
        let pt = board.point(x, y);
        board.setup_stone(pt, colour);
        wanted.push((pt, colour.cell()));
    }
    let ko = ko
        .as_deref()
        .and_then(|v| crate::coords::parse_move(&board, v))
        .and_then(|mv| match mv {
            Move::Play(pt) => Some(pt),
            _ => None,
        });
    board.resume(to_move, ko);
    let dropped = wanted
        .iter()
        .filter(|&&(pt, cell)| board.cell(pt) != cell)
        .map(|&(pt, _)| format_move(&board, Move::Play(pt)))
        .collect();
    let seen = seen.iter().filter_map(|rows| rows_hash(size, rows));
    Ok(Request {
        game: Game::resumed(board, komi, seen),
        millis: (seconds.clamp(0.2, 60.0) * 1000.0) as u64,
        move_first,
        dropped,
    })
}

/// The positional hash of a board written as rows, top-first — the form the
/// page keeps its history in. `None` for anything that is not a position.
fn rows_hash(size: usize, rows: &str) -> Option<u64> {
    if rows.split('/').count() != size {
        return None;
    }
    let mut board = Board::new(size);
    for (i, row) in rows.split('/').enumerate() {
        if row.chars().count() != size {
            return None;
        }
        for (x, ch) in row.chars().enumerate() {
            let colour = match ch {
                'b' => Color::Black,
                'w' => Color::White,
                '.' => continue,
                _ => return None,
            };
            let pt = board.point(x, size - 1 - i);
            board.setup_stone(pt, colour);
        }
    }
    Some(board.hash())
}

/// The stones, rows top-first: the form the page draws from and sends back.
fn board_rows(board: &Board) -> String {
    let size = board.size();
    let mut out = String::new();
    for y in (0..size).rev() {
        for x in 0..size {
            out.push(match board.cell(board.point(x, y)) {
                Cell::Black => 'b',
                Cell::White => 'w',
                _ => '.',
            });
        }
        if y > 0 {
            out.push('/');
        }
    }
    out
}

/// The board as the rules leave it, for the page to draw, and with it the ko
/// point if the last capture left one. The two travel together so that no
/// answer can carry the position without it.
fn board_line(board: &Board) -> String {
    let mut out = format!("board {} {}\n", board.size(), board_rows(board));
    if let Some(pt) = board.ko_point() {
        out.push_str(&format!("ko {}\n", format_move(board, Move::Play(pt))));
    }
    out
}

/// Why a move was refused, in the page's voice.
fn refusal(why: Rejected) -> String {
    match why {
        Rejected::Rules(why) => format!("that move is {why:?}"),
        Rejected::Superko => "that repeats a position this game has already been in".to_string(),
    }
}

fn best_move(
    board: &Board,
    komi: f32,
    millis: u64,
    opts: &ServeOptions,
    engine: Option<&mut GtpEngine>,
) -> Result<(Move, String), String> {
    match engine {
        Some(engine) => engine
            .advise(board, komi, millis)
            .map(|a| (a.best, a.source)),
        None => {
            let mut params = opts.params;
            params.komi = komi;
            params.time_limit = Some(std::time::Duration::from_millis(millis));
            params.playouts = None;
            let result = mcts::search(board, &params);
            Ok((result.best, format!("gobot, {} playouts", result.playouts)))
        }
    }
}

/// Plays the person's move and nothing else.
///
/// This is separate from `/play` so that a stone appears the moment it is
/// tapped. Doing both halves of a turn in one request meant the board sat
/// unchanged for however long the engine thought, which reads as a dropped tap.
/// Splitting it also keeps the rules in charge of the instant half: a capture
/// shows straight away and correctly, which drawing it optimistically in the
/// page could not manage without reimplementing the rules there.
fn make_move(body: &str, opts: &ServeOptions) -> String {
    let mut request = match parse_request(body, opts) {
        Ok(r) => r,
        Err(why) => return format!("error {why}\n"),
    };
    let Some(vertex) = request.move_first.clone() else {
        return "error no move given\n".to_string();
    };
    let Some(mv) = crate::coords::parse_move(request.board(), &vertex) else {
        return format!("error `{vertex}` is not a point on this board\n");
    };
    if let Err(why) = request.game.play(mv) {
        return format!("{}error {}\n", board_line(request.board()), refusal(why));
    }
    let mut out = board_line(request.board());
    out.push_str(&format!("last {}\n", format_move(request.board(), mv)));
    out.push_str(&format!(
        "tomove {}\n",
        if request.board().to_move() == Color::Black {
            "b"
        } else {
            "w"
        }
    ));
    out
}

/// One turn of a game: play the move the person made, then answer it.
fn play(body: &str, opts: &ServeOptions, engine: Option<&mut GtpEngine>) -> String {
    let mut request = match parse_request(body, opts) {
        Ok(r) => r,
        Err(why) => return format!("error {why}\n"),
    };
    let mut out = String::new();
    if let Some(vertex) = &request.move_first {
        let Some(mv) = crate::coords::parse_move(request.board(), vertex) else {
            return format!("error `{vertex}` is not a point on this board\n");
        };
        if let Err(why) = request.game.play(mv) {
            return format!("{}error {}\n", board_line(request.board()), refusal(why));
        }
    }
    if request
        .board()
        .points()
        .all(|pt| request.board().cell(pt) != Cell::Empty)
    {
        return format!("{}error the board is full\n", board_line(request.board()));
    }
    let komi = request.game.komi;
    match best_move(request.board(), komi, request.millis, opts, engine) {
        Ok((mv, source)) => {
            // The search works a bare board and knows nothing of the history,
            // so its move can still repeat a position. Passing is the honest
            // answer there — a pass can never repeat one — where calling it
            // illegal would leave the turn with the engine and the game stuck.
            let played = match request.game.play(mv) {
                Ok(()) => Some(mv),
                Err(Rejected::Superko) => {
                    let _ = request.game.play(Move::Pass);
                    Some(Move::Pass)
                }
                Err(Rejected::Rules(_)) => None,
            };
            out.push_str(&board_line(request.board()));
            match played {
                Some(mv) => {
                    let name = format_move(request.board(), mv);
                    out.push_str(&format!("played {name}\n"));
                    out.push_str(&format!("last {name}\n"));
                    out.push_str(&format!("source {source}\n"));
                }
                None => out.push_str(&format!(
                    "error the engine offered {}, which is illegal\n",
                    format_move(request.board(), mv)
                )),
            }
        }
        Err(why) => {
            out.push_str(&board_line(request.board()));
            out.push_str(&format!("error {why}\n"));
        }
    }
    out.push_str(&format!(
        "tomove {}\n",
        if request.board().to_move() == Color::Black {
            "b"
        } else {
            "w"
        }
    ));
    out
}

fn analyse(body: &str, opts: &ServeOptions, engine: Option<&mut GtpEngine>) -> String {
    let request = match parse_request(body, opts) {
        Ok(r) => r,
        Err(why) => return format!("error {why}\n"),
    };
    let board = *request.board();
    let komi = request.game.komi;

    let mut out = board_line(&board);
    if !request.dropped.is_empty() {
        out.push_str(&format!("dropped {}\n", request.dropped.join(" ")));
    }
    if board.points().all(|pt| board.cell(pt) != Cell::Empty) {
        out.push_str("error the board is full\n");
        return out;
    }
    let millis = request.millis;
    match engine {
        Some(engine) => match engine.advise(&board, komi, millis) {
            Ok(advice) => {
                out.push_str(&format!("source {}\n", advice.source));
                if advice.candidates.is_empty() {
                    // An engine with no analysis command offers a move and
                    // nothing else. Sending it as a candidate with a win rate
                    // of zero would read as "losing", which is not what an
                    // absent number means.
                    out.push_str(&format!("best {}\n", format_move(&board, advice.best)));
                }
                for c in advice.candidates.iter().take(6) {
                    let pv: Vec<String> = c.pv.iter().map(|m| format_move(&board, *m)).collect();
                    out.push_str(&format!(
                        "move {} {:.4} {} {}\n",
                        format_move(&board, c.mv),
                        c.winrate,
                        c.visits,
                        pv.join(" ")
                    ));
                }
            }
            Err(why) => out.push_str(&format!("error {why}\n")),
        },
        None => {
            let mut params = opts.params;
            params.komi = komi;
            params.time_limit = Some(std::time::Duration::from_millis(millis));
            params.playouts = None;
            let result = mcts::search(&board, &params);
            out.push_str(&format!(
                "source gobot, {} playouts in {:.1}s on {} threads\n",
                result.playouts,
                result.elapsed.as_secs_f64(),
                result.threads
            ));
            for c in result.candidates.iter().take(6).filter(|c| c.visits > 0) {
                let pv: Vec<String> = c.pv.iter().map(|m| format_move(&board, *m)).collect();
                out.push_str(&format!(
                    "move {} {:.4} {} {}\n",
                    format_move(&board, c.mv),
                    c.winrate(),
                    c.visits,
                    pv.join(" ")
                ));
            }
        }
    }
    out
}

/// The addresses this machine answers on, so the page can be opened from a
/// phone without anyone having to go and look them up. Tailscale's 100.64/10
/// range is listed first when it is there, because reaching the board over a
/// tailnet is both easier and narrower than opening it to a whole network.
fn local_addresses() -> Vec<String> {
    let Ok(out) = std::process::Command::new("ifconfig").arg("-a").output() else {
        return vec!["127.0.0.1".to_string()];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut tailnet = Vec::new();
    let mut lan = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("inet ") else {
            continue;
        };
        let Some(ip) = rest.split_whitespace().next() else {
            continue;
        };
        if ip == "127.0.0.1" || ip.contains(':') {
            continue;
        }
        // 100.64.0.0/10, which is what a tailnet hands out.
        let is_tailnet = ip
            .split_once('.')
            .and_then(|(a, rest)| {
                Some((
                    a.parse::<u8>().ok()?,
                    rest.split('.').next()?.parse::<u8>().ok()?,
                ))
            })
            .is_some_and(|(a, b)| a == 100 && (64..128).contains(&b));
        if is_tailnet {
            tailnet.push(ip.to_string());
        } else {
            lan.push(ip.to_string());
        }
    }
    tailnet.extend(lan);
    tailnet.push("127.0.0.1".to_string());
    tailnet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ServeOptions {
        ServeOptions {
            bind: "127.0.0.1".to_string(),
            port: 0,
            size: 9,
            komi: 6.5,
            params: Params {
                playouts: Some(2_000),
                time_limit: None,
                threads: 2,
                ..Params::default()
            },
            engine: None,
        }
    }

    fn line<'a>(text: &'a str, head: &str) -> Option<&'a str> {
        text.lines().find(|l| l.starts_with(head))
    }

    #[test]
    fn the_board_it_sends_back_is_the_one_the_rules_allow() {
        // The page draws whatever comes back, so this is the contract that
        // keeps the two in step. A white stone surrounded on all four sides
        // cannot be there, and the answer has to say so rather than quietly
        // drawing a board nobody asked for.
        let body = "size 9\nkomi 6.5\ntime 0.3\ntomove b\n\
                    w 4 4\nb 4 5\nb 4 3\nb 3 4\nb 5 4\n";
        let mut opts = options();
        opts.params.playouts = Some(200);
        let out = analyse(body, &opts, None);
        let board = line(&out, "board ").expect("a board line");
        let rows: Vec<&str> = board
            .split_whitespace()
            .nth(2)
            .unwrap()
            .split('/')
            .collect();
        assert_eq!(rows.len(), 9);
        // Row 5 counted from the bottom is index 4 from the top.
        assert_eq!(
            rows[4].chars().nth(4),
            Some('.'),
            "the surrounded white stone should be gone: {board}"
        );
        assert_eq!(rows[3].chars().nth(4), Some('b'), "black e6 stays");
        assert!(
            line(&out, "dropped ").is_some_and(|l| l.contains("E5")),
            "and it should be named: {out}"
        );
    }

    #[test]
    fn it_answers_a_plain_position_with_moves() {
        let out = analyse("size 9\ntime 0.3\ntomove b\nb 2 2\n", &options(), None);
        assert!(line(&out, "source ").is_some(), "{out}");
        let moves: Vec<&str> = out.lines().filter(|l| l.starts_with("move ")).collect();
        assert!(!moves.is_empty(), "no suggestions came back: {out}");
        for m in &moves {
            let w: Vec<&str> = m.split_whitespace().collect();
            assert!(
                w.len() >= 4,
                "a move line is `move vertex winrate visits pv`: {m}"
            );
            let winrate: f64 = w[2].parse().expect("winrate is a number");
            assert!((0.0..=1.0).contains(&winrate), "{m}");
        }
    }

    #[test]
    fn a_turn_plays_my_move_and_then_answers_it() {
        let mut opts = options();
        opts.params.playouts = Some(400);
        let out = play("size 9\ntime 0.3\ntomove b\nmove E5\n", &opts, None);
        let board = line(&out, "board ").expect("a board");
        let rows: Vec<&str> = board
            .split_whitespace()
            .nth(2)
            .unwrap()
            .split('/')
            .collect();
        assert_eq!(
            rows[4].chars().nth(4),
            Some('b'),
            "my stone is on E5: {board}"
        );
        let played = line(&out, "played ").expect("the engine answered");
        let answer = played.split_whitespace().nth(1).unwrap();
        assert_ne!(answer, "E5", "it cannot answer on the point I just took");
        let stones: usize = rows
            .iter()
            .map(|r| r.chars().filter(|c| *c != '.').count())
            .sum();
        assert_eq!(stones, 2, "one move each: {board}");
        assert_eq!(
            line(&out, "tomove "),
            Some("tomove b"),
            "and it is my turn again"
        );
    }

    #[test]
    fn a_move_lands_on_its_own_without_waiting_for_the_engine() {
        // The half of a turn that has to be instant: the rules apply it, the
        // capture is resolved, and no search runs.
        let body = "size 9\ntomove b\nw 4 4\nb 4 5\nb 4 3\nb 5 4\nmove D5\n";
        let out = make_move(body, &options());
        let board = line(&out, "board ").expect("a board");
        let rows: Vec<&str> = board
            .split_whitespace()
            .nth(2)
            .unwrap()
            .split('/')
            .collect();
        assert_eq!(
            rows[4].chars().nth(4),
            Some('.'),
            "black surrounded the white stone, so it should be gone: {board}"
        );
        assert_eq!(line(&out, "last "), Some("last D5"));
        assert_eq!(line(&out, "tomove "), Some("tomove w"));
        assert!(
            line(&out, "played ").is_none(),
            "the engine has not moved yet"
        );
    }

    #[test]
    fn a_move_on_its_own_is_still_checked() {
        let out = make_move("size 9\ntomove w\nb 4 4\nmove E5\n", &options());
        assert!(line(&out, "error ").is_some(), "{out}");
        assert!(line(&out, "last ").is_none(), "{out}");
    }

    #[test]
    fn a_turn_refuses_a_move_the_rules_refuse() {
        // E5 is taken, so playing it again is not a turn that can happen.
        let out = play(
            "size 9\ntime 0.3\ntomove w\nb 4 4\nmove E5\n",
            &options(),
            None,
        );
        assert!(line(&out, "error ").is_some(), "{out}");
        assert!(
            line(&out, "played ").is_none(),
            "and the engine must not move on top of a refused turn: {out}"
        );
    }

    /// The ko shape, as the page would send it: black e6 d5 e4, white f6 e5
    /// g5 f4, with black to play f5 and take the lone white stone.
    const KO_STONES: &str = "b 4 5\nb 3 4\nb 4 3\nw 5 5\nw 4 4\nw 6 4\nw 5 3\n";
    /// The same position after black has taken, which is what white would be
    /// recreating.
    const KO_AFTER_TAKE: &str = "b 4 5\nb 3 4\nb 4 3\nb 5 4\nw 5 5\nw 6 4\nw 5 3\n";
    const KO_BEFORE: &str =
        "........./........./........./....bw.../...bw.w../....bw.../........./........./.........";

    /// White answering the take at E5 — the retake — with whatever `extra`
    /// lines the page would have carried about the game so far.
    fn white_retakes(extra: &str) -> String {
        make_move(
            &format!("size 9\ntomove w\n{extra}{KO_AFTER_TAKE}move E5\n"),
            &options(),
        )
    }

    #[test]
    fn a_ko_survives_a_request_that_rebuilds_the_board() {
        // Every request builds the board from stones alone, and a position has
        // no memory of the capture that made it. The ko point therefore has to
        // travel with the request; when it did not, the retake below was legal
        // and the engine would offer it.
        let out = make_move(
            &format!("size 9\ntomove b\n{KO_STONES}move F5\n"),
            &options(),
        );
        assert_eq!(
            line(&out, "ko "),
            Some("ko E5"),
            "the take leaves a ko: {out}"
        );

        let held = white_retakes("ko E5\n");
        assert_eq!(
            line(&held, "error "),
            Some("error that move is Ko"),
            "white must not take straight back: {held}"
        );

        // Without the ko point the same move is legal, which is what makes the
        // test above about the ko and not about the move.
        let free = white_retakes("");
        assert_eq!(line(&free, "last "), Some("last E5"), "{free}");
    }

    #[test]
    fn the_engine_is_not_offered_the_ko_point_either() {
        let mut opts = options();
        opts.params.playouts = Some(3_000);
        let out = analyse(
            &format!("size 9\ntomove w\nko E5\n{KO_AFTER_TAKE}"),
            &opts,
            None,
        );
        assert_eq!(line(&out, "ko "), Some("ko E5"), "{out}");
        for l in out.lines().filter(|l| l.starts_with("move ")) {
            assert_ne!(
                l.split_whitespace().nth(1),
                Some("E5"),
                "the search was handed a board that let it suggest the ko point"
            );
        }
    }

    #[test]
    fn superko_refuses_a_position_the_game_has_already_been_in() {
        // Simple ko only looks one move back. The history the request carries
        // is what catches a repeat that arrives by a longer road.
        let repeat = white_retakes(&format!("seen {KO_BEFORE}\n"));
        assert_eq!(
            line(&repeat, "error "),
            Some("error that repeats a position this game has already been in"),
            "{repeat}"
        );

        // A history that does not contain this position leaves the move alone.
        let elsewhere = white_retakes(
            "seen bbbbbbbbb/........./........./........./........./........./........./........./.........\n",
        );
        assert_eq!(line(&elsewhere, "last "), Some("last E5"), "{elsewhere}");
    }

    #[test]
    fn the_engine_passes_rather_than_stall_when_its_move_would_repeat() {
        // The search works a bare board and does not know the history, so it
        // can offer a move superko refuses. Answering "illegal" left the turn
        // with the engine and the game with nowhere to go.
        let mut opts = options();
        opts.params.playouts = Some(2_000);
        // Black on every point but the right-hand two. Both plays white has
        // recreate a position the game has already stood in.
        let out = play(
            "size 3\ntomove w\nb 0 0\nb 1 0\nb 2 0\nb 0 1\nb 1 1\nb 0 2\nb 1 2\n\
             seen bbw/bb./bbb\nseen bb./bbw/bbb\n",
            &opts,
            None,
        );
        assert_eq!(line(&out, "played "), Some("played pass"), "{out}");
        assert_eq!(
            line(&out, "tomove "),
            Some("tomove b"),
            "the turn has to move on: {out}"
        );
    }

    #[test]
    fn a_board_size_it_cannot_play_is_refused_rather_than_clamped() {
        let out = analyse("size 40\ntomove b\n", &options(), None);
        assert!(line(&out, "error ").is_some(), "{out}");
        assert!(
            line(&out, "move ").is_none(),
            "and nothing is suggested: {out}"
        );
    }
}
