//! The terminal board. You enter the position your app is showing, then enter
//! your opponent's moves as they come, and the engine says what it would play.

use std::io::{BufRead, Write};
use std::time::Duration;

use gobot::board::{Board, Cell, Color, Move};
use gobot::coords::{diagram, format_move, parse_diagram, parse_diagram_report, parse_move};
use gobot::game::{Game, Rejected};
use gobot::mcts::{self, Params};

const USAGE: &str = "\
gobot — a Go engine for 9x9, and a second opinion on your own games.

  gobot [play]        the terminal board (default)
  gobot serve         a board in a browser, served from this machine
  gobot gtp           speak GTP on stdin/stdout, for Sabaki or gogui-twogtp
  gobot bench         how many playouts a second this machine manages
  gobot match         play a series against another GTP engine and count them
  gobot help

Options
  --size <n>          board size, 2..19 (default 9)
  --komi <x>          komi for White (default 7.5)
  --time <secs>       thinking time per hint (default 3)
  --playouts <n>      fixed playout count instead of a time limit
  --threads <n>       search threads (default: cores, capped at 8)
  --me black|white    which side the hints are for (default black)
  --diagram <file>    start from a position written as a diagram
  --handicap <n>      start with n handicap stones for Black, White to move
  --engine <cmd>      take hints from another GTP engine instead of the
                      built-in search, e.g. a KataGo command line

For `serve`
  --port <n>          which port to listen on (default 8080)
  --bind <addr>       127.0.0.1 by default; 0.0.0.0 to reach it from a tablet
                      or phone on the same network

For `match`
  --against <cmd>     the opponent, a command that speaks GTP on stdin/stdout
  --games <n>         how many to play, colours alternating (default 20)
  --handicap <n>      stones to gobot; it then takes Black every game
  --verbose           print the board after every move
";

const COMMANDS: &str = "\
  <vertex>            play for whoever is to move, e.g. e5
  b <vertex>          play a Black move  (also: black e5)
  w <vertex>          play a White move
  pass                pass for whoever is to move
  hint [secs]         think and print the best moves  (also: h)
  show                print the board  (also: s)
  undo                take back the last move  (also: u)
  add b|w <vertex>    place a stone without it being a move
  del <vertex>        take a stone off the board
  setup               read a position as a diagram, ended by a blank line
  handicap <n>        place the conventional 2-9 handicap stones for Black
  turn b|w            say whose turn it is
  me b|w              which side to give hints for
  komi <x>            set komi
  time <secs>         set thinking time
  score               area score of the position as it stands
  help                this list
  quit                leave
";

struct Options {
    size: usize,
    params: Params,
    me: Color,
    diagram: Option<String>,
    mode: String,
    against: Option<String>,
    port: u16,
    bind: String,
    games: usize,
    verbose: bool,
    handicap: usize,
    engine: Option<String>,
}

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("gobot: {msg}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    match opts.mode.as_str() {
        "help" | "--help" | "-h" => print!("{USAGE}"),
        "gtp" => {
            if let Err(e) = gobot::gtp::run(opts.size, opts.params) {
                eprintln!("gobot: {e}");
                std::process::exit(1);
            }
        }
        "bench" => bench(&opts),
        "match" => run_match(&opts),
        "serve" => {
            let settings = gobot::web::ServeOptions {
                bind: opts.bind.clone(),
                port: opts.port,
                size: opts.size,
                komi: opts.params.komi,
                params: opts.params,
                engine: opts.engine.clone(),
            };
            if let Err(e) = gobot::web::serve(settings) {
                eprintln!("gobot: {e}");
                std::process::exit(1);
            }
        }
        "play" => play(opts),
        other => {
            eprintln!("gobot: unknown mode `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn parse_args() -> Result<Options, String> {
    let mut opts = Options {
        size: 9,
        params: Params::default(),
        me: Color::Black,
        diagram: None,
        mode: "play".to_string(),
        against: None,
        port: 8080,
        bind: "127.0.0.1".to_string(),
        games: 20,
        verbose: false,
        handicap: 0,
        engine: None,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let mut mode_set = false;
    while i < args.len() {
        let a = args[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a {
            "--size" => opts.size = value("--size")?.parse().map_err(|_| "bad --size")?,
            "--komi" => opts.params.komi = value("--komi")?.parse().map_err(|_| "bad --komi")?,
            "--time" => {
                let secs: f64 = value("--time")?.parse().map_err(|_| "bad --time")?;
                opts.params.time_limit = Some(Duration::from_secs_f64(secs));
                opts.params.playouts = None;
            }
            "--playouts" => {
                opts.params.playouts =
                    Some(value("--playouts")?.parse().map_err(|_| "bad --playouts")?);
                opts.params.time_limit = None;
            }
            "--threads" => {
                opts.params.threads = value("--threads")?.parse().map_err(|_| "bad --threads")?
            }
            "--me" => opts.me = parse_colour(&value("--me")?).ok_or("bad --me")?,
            "--diagram" => opts.diagram = Some(value("--diagram")?),
            "--against" => opts.against = Some(value("--against")?),
            "--engine" => opts.engine = Some(value("--engine")?),
            "--port" => opts.port = value("--port")?.parse().map_err(|_| "bad --port")?,
            "--bind" => opts.bind = value("--bind")?,
            "--games" => opts.games = value("--games")?.parse().map_err(|_| "bad --games")?,
            "--verbose" => opts.verbose = true,
            "--handicap" => {
                opts.handicap = value("--handicap")?.parse().map_err(|_| "bad --handicap")?
            }
            "--help" | "-h" | "help" => {
                opts.mode = "help".to_string();
                mode_set = true;
            }
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other if !mode_set => {
                opts.mode = other.to_string();
                mode_set = true;
            }
            other => return Err(format!("unexpected argument `{other}`")),
        }
        i += 1;
    }
    if !(2..=gobot::board::MAX_SIZE).contains(&opts.size) {
        return Err(format!("--size must be 2..{}", gobot::board::MAX_SIZE));
    }
    Ok(opts)
}

fn parse_colour(s: &str) -> Option<Color> {
    match s.to_ascii_lowercase().as_str() {
        "b" | "black" | "x" => Some(Color::Black),
        "w" | "white" | "o" => Some(Color::White),
        _ => None,
    }
}

fn bench(opts: &Options) {
    let board = Board::new(opts.size);
    let mut params = opts.params;
    params.playouts = Some(200_000);
    params.time_limit = None;
    println!(
        "{}x{}, {} thread(s), {} playouts",
        opts.size,
        opts.size,
        params.threads,
        params.playouts.unwrap()
    );
    let result = mcts::search(&board, &params);
    let per_sec = result.playouts as f64 / result.elapsed.as_secs_f64();
    println!(
        "{} playouts in {:.2}s = {:.0}/s total, {:.0}/s per thread",
        result.playouts,
        result.elapsed.as_secs_f64(),
        per_sec,
        per_sec / result.threads as f64
    );
    println!("best: {}", format_move(&board, result.best));
}

fn run_match(opts: &Options) {
    let Some(command) = opts.against.as_deref() else {
        eprintln!("gobot: match needs --against \"<a command that speaks GTP>\"");
        eprintln!("       e.g. --against \"gnugo --mode gtp --chinese-rules --capture-all-dead\"");
        std::process::exit(2);
    };
    let settings = gobot::arena::MatchSettings {
        games: opts.games,
        size: opts.size,
        komi: opts.params.komi,
        params: opts.params,
        verbose: opts.verbose,
        handicap: opts.handicap,
    };
    if let Err(why) = gobot::arena::run_match(command, &settings) {
        eprintln!("gobot: {why}");
        std::process::exit(1);
    }
}

fn play(opts: Options) {
    let mut params = opts.params;
    let mut me = opts.me;
    let mut game = match &opts.diagram {
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|text| parse_diagram(&text, opts.size))
        {
            Ok(mut board) => {
                board.set_to_move(me);
                Game::from_board(board, params.komi)
            }
            Err(why) => {
                eprintln!("gobot: {path}: {why}");
                std::process::exit(1);
            }
        },
        None => Game::new(opts.size, params.komi),
    };

    let mut advisor = match &opts.engine {
        Some(command) => match gobot::arena::GtpEngine::spawn(command) {
            Ok(engine) => {
                println!(
                    "hints from {} — the built-in search is not used",
                    engine.label
                );
                Some(engine)
            }
            Err(why) => {
                eprintln!("gobot: {why}");
                std::process::exit(1);
            }
        },
        None => None,
    };

    if opts.handicap > 0
        && let Err(why) = place_handicap(&mut game, opts.size, opts.handicap, params.komi)
    {
        eprintln!("gobot: --handicap: {why}");
        std::process::exit(2);
    }

    println!(
        "gobot {} — {}x{}, komi {}, hints for {}, {} per hint",
        env!("CARGO_PKG_VERSION"),
        opts.size,
        opts.size,
        params.komi,
        me.name(),
        budget_text(&params)
    );
    println!("`help` for the commands, `setup` to enter a position.\n");
    print!("{}", diagram(game.board(), &[]));

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        prompt(&game, me);
        let Some(Ok(line)) = lines.next() else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut words = line.split_whitespace();
        let head = words.next().unwrap_or("").to_ascii_lowercase();
        let rest: Vec<&str> = words.collect();

        match head.as_str() {
            "quit" | "q" | "exit" => break,
            "help" | "?" => print!("{COMMANDS}"),
            "show" | "s" => print!("{}", diagram(game.board(), &[])),
            "score" => {
                let s = game.score();
                println!("{}", score_text(s));
            }
            "undo" | "u" => {
                if game.undo() {
                    print!("{}", diagram(game.board(), &[]));
                } else {
                    println!("nothing to undo");
                }
            }
            "komi" => match rest.first().and_then(|v| v.parse::<f32>().ok()) {
                Some(k) => {
                    params.komi = k;
                    game.komi = k;
                    println!("komi {k}");
                }
                None => println!("komi <number>"),
            },
            "time" => match rest.first().and_then(|v| v.parse::<f64>().ok()) {
                Some(secs) if secs > 0.0 => {
                    params.time_limit = Some(Duration::from_secs_f64(secs));
                    params.playouts = None;
                    println!("thinking {secs}s per hint");
                }
                _ => println!("time <seconds>"),
            },
            "me" => match rest.first().and_then(|v| parse_colour(v)) {
                Some(c) => {
                    me = c;
                    println!("hints for {}", c.name());
                }
                None => println!("me black|white"),
            },
            "handicap" => match rest.first().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) => match place_handicap(&mut game, opts.size, n, params.komi) {
                    Ok(()) => {
                        print!("{}", diagram(game.board(), &[]));
                        println!("{} stones for black; white to move", n);
                    }
                    Err(why) => println!("handicap: {why}"),
                },
                None => println!("handicap <2..9>"),
            },
            "turn" => match rest.first().and_then(|v| parse_colour(v)) {
                Some(c) => {
                    game.set_to_move(c);
                    println!("{} to move", c.name());
                }
                None => println!("turn black|white"),
            },
            "hint" | "h" => {
                let mut p = params;
                if let Some(secs) = rest.first().and_then(|v| v.parse::<f64>().ok()) {
                    p.time_limit = Some(Duration::from_secs_f64(secs));
                    p.playouts = None;
                }
                hint(&game, &p, advisor.as_mut());
            }
            "add" => add_stone(&mut game, &rest),
            "del" | "remove" => match rest.first().and_then(|v| parse_move(game.board(), v)) {
                Some(Move::Play(pt)) => {
                    let mut board = *game.board();
                    board.remove_stone(pt);
                    let turn = game.to_move();
                    game = Game::from_board(board, params.komi);
                    game.set_to_move(turn);
                    print!("{}", diagram(game.board(), &[]));
                }
                _ => println!("del <vertex>"),
            },
            "setup" => match read_diagram(&mut lines, opts.size) {
                Ok((mut board, dropped)) => {
                    report_dropped(&board, &dropped);
                    board.set_to_move(me);
                    game = Game::from_board(board, params.komi);
                    print!("{}", diagram(game.board(), &[]));
                    println!("{} to move", me.name());
                }
                Err(why) => println!("setup: {why}"),
            },
            "pass" => make_move(&mut game, Move::Pass, None),
            "b" | "black" | "w" | "white" => {
                let colour = parse_colour(&head).unwrap();
                match rest.first().and_then(|v| parse_move(game.board(), v)) {
                    Some(mv) => make_move(&mut game, mv, Some(colour)),
                    None => println!("{head} <vertex>"),
                }
            }
            _ => match parse_move(game.board(), &line) {
                Some(mv) => make_move(&mut game, mv, None),
                None => println!("not a command or a vertex: `{line}` (try `help`)"),
            },
        }

        // The point of the tool: when it comes round to you, it has already
        // thought about it.
        if game.to_move() == me && matches!(head.as_str(), "b" | "black" | "w" | "white" | "pass") {
            hint(&game, &params, advisor.as_mut());
        }
    }
}

fn prompt(game: &Game, me: Color) {
    let turn = game.to_move();
    let who = if turn == me { "you" } else { "them" };
    print!("{} ({who}) > ", turn.name());
    let _ = std::io::stdout().flush();
}

fn budget_text(params: &Params) -> String {
    match (params.time_limit, params.playouts) {
        (Some(d), _) => format!("{:.1}s", d.as_secs_f64()),
        (None, Some(n)) => format!("{n} playouts"),
        _ => "nothing".to_string(),
    }
}

fn make_move(game: &mut Game, mv: Move, colour: Option<Color>) {
    if let Some(c) = colour {
        game.set_to_move(c);
    }
    match game.play(mv) {
        Ok(()) => print!("{}", diagram(game.board(), &[])),
        Err(Rejected::Superko) => println!("illegal: that repeats an earlier position (superko)"),
        Err(Rejected::Rules(why)) => println!("illegal: {}", reason_text(why)),
    }
}

fn reason_text(why: gobot::board::Illegal) -> &'static str {
    use gobot::board::Illegal::*;
    match why {
        NotEmpty => "there is already a stone there",
        Suicide => "that stone would have no liberties",
        Ko => "that is the ko point",
        OffBoard => "that is off the board",
    }
}

fn add_stone(game: &mut Game, rest: &[&str]) {
    let Some(colour) = rest.first().and_then(|v| parse_colour(v)) else {
        println!("add b|w <vertex> [<vertex> ...]");
        return;
    };
    let mut placed = 0;
    for v in &rest[1..] {
        match parse_move(game.board(), v) {
            Some(Move::Play(pt)) => {
                game.setup_stone(pt, colour);
                placed += 1;
            }
            _ => println!("not a vertex: `{v}`"),
        }
    }
    if placed > 0 {
        print!("{}", diagram(game.board(), &[]));
    }
}

/// Clears the board and lays out the conventional handicap stones. White moves
/// first afterwards, which is what handicap means.
fn place_handicap(game: &mut Game, size: usize, stones: usize, komi: f32) -> Result<(), String> {
    let points = gobot::coords::handicap_points(size, stones)?;
    let mut board = Board::new(size);
    for (x, y) in points {
        board.setup_stone(board.point(x, y), Color::Black);
    }
    board.set_to_move(Color::White);
    *game = Game::from_board(board, komi);
    Ok(())
}

fn report_dropped(board: &Board, dropped: &[gobot::board::Point]) {
    if dropped.is_empty() {
        return;
    }
    let names: Vec<String> = dropped
        .iter()
        .map(|&pt| format_move(board, Move::Play(pt)))
        .collect();
    println!(
        "warning: {} could not be on the board — a stone there would have no\n\
         liberties, so it was left off. Check those points against your screen.",
        names.join(", ")
    );
}

fn read_diagram(
    lines: &mut impl Iterator<Item = std::io::Result<String>>,
    size: usize,
) -> Result<(Board, Vec<gobot::board::Point>), String> {
    println!(
        "Paste {size} rows, top row first. X or # is Black, O is White, . is empty.\n\
         Column letters and row numbers are ignored. Blank line when done."
    );
    let mut text = String::new();
    let mut rows = 0;
    for line in lines.by_ref() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            break;
        }
        text.push_str(&line);
        text.push('\n');
        rows += 1;
        if rows == size {
            break;
        }
    }
    parse_diagram_report(&text, size)
}

fn score_text(score: f32) -> String {
    if score > 0.0 {
        format!("Black by {score:.1}")
    } else if score < 0.0 {
        format!("White by {:.1}", -score)
    } else {
        "dead even".to_string()
    }
}

fn hint(game: &Game, params: &Params, advisor: Option<&mut gobot::arena::GtpEngine>) {
    let board = game.board();
    let colour = board.to_move();
    if board.points().all(|pt| board.cell(pt) != Cell::Empty) {
        println!("the board is full");
        return;
    }
    if let Some(engine) = advisor {
        let millis = params
            .time_limit
            .map_or(2000, |d| d.as_millis().max(200) as u64);
        match engine.advise(board, params.komi, millis) {
            Ok(advice) => print_external(board, colour, &advice),
            Err(why) => println!("the engine did not answer: {why}"),
        }
        return;
    }
    let result = mcts::search(board, params);
    let Some(best) = result.candidates.first() else {
        println!("no legal move but to pass");
        return;
    };

    let marks: Vec<_> = match best.mv {
        Move::Play(pt) => vec![pt],
        _ => vec![],
    };
    print!("{}", diagram(board, &marks));
    println!(
        "{} to play — {} playouts in {:.1}s on {} thread(s)",
        colour.name(),
        thousands(result.playouts),
        result.elapsed.as_secs_f64(),
        result.threads
    );
    for (i, c) in result.candidates.iter().take(5).enumerate() {
        if c.visits == 0 {
            break;
        }
        let pv: Vec<String> = c.pv.iter().map(|m| format_move(board, *m)).collect();
        println!(
            "  {} {:<4} {:>5.1}%  {:>9} visits   {}",
            if i == 0 { "->" } else { "  " },
            format_move(board, c.mv),
            c.winrate() * 100.0,
            thousands(c.visits),
            pv.join(" ")
        );
    }
    let wr = best.winrate();
    if wr < 0.15 {
        println!("  (it does not think {} is winning this)", colour.name());
    } else if wr > 0.85 {
        println!("  ({} is well ahead here)", colour.name());
    }
}

fn print_external(board: &Board, colour: Color, advice: &gobot::arena::Advice) {
    let marks: Vec<_> = match advice.best {
        Move::Play(pt) => vec![pt],
        _ => vec![],
    };
    print!("{}", diagram(board, &marks));
    if advice.candidates.is_empty() {
        println!(
            "{} to play — {} says {}",
            colour.name(),
            advice.source,
            format_move(board, advice.best)
        );
        return;
    }
    println!("{} to play — {}", colour.name(), advice.source);
    for (i, c) in advice.candidates.iter().take(5).enumerate() {
        let pv: Vec<String> = c.pv.iter().map(|m| format_move(board, *m)).collect();
        println!(
            "  {} {:<4} {:>5.1}%  {:>9} visits   {}",
            if i == 0 { "->" } else { "  " },
            format_move(board, c.mv),
            c.winrate * 100.0,
            thousands(c.visits),
            pv.join(" ")
        );
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
