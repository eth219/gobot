//! Plays gobot against another GTP engine and counts the games.
//!
//! This exists because "is it stronger?" has no other honest answer. A hint
//! that looks sensible is not evidence; a win rate over a few hundred games
//! against a fixed opponent is.
//!
//! Scoring is this engine's own Tromp-Taylor area count, which is only right if
//! the game is played out until every dead stone has actually been captured.
//! Opponents that pass over dead stones must be told not to — for GNU Go that
//! is `--capture-all-dead`, and getting it wrong shows up as scores that
//! disagree wildly with the board.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::board::{Color, Move};
use crate::coords::{format_move, parse_move};
use crate::game::Game;
use crate::mcts::{self, Params};

pub struct GtpEngine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub label: String,
}

impl GtpEngine {
    /// Starts `command` under a shell, so that it can carry its own arguments.
    pub fn spawn(command: &str) -> Result<GtpEngine, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start `{command}`: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut engine = GtpEngine {
            child,
            stdin,
            stdout,
            label: command.to_string(),
        };
        if let Ok(name) = engine.send("name")
            && !name.is_empty()
        {
            engine.label = name;
        }
        Ok(engine)
    }

    pub fn send(&mut self, command: &str) -> Result<String, String> {
        writeln!(self.stdin, "{command}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut body = String::new();
        let mut started = false;
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| format!("reading `{command}`: {e}"))?;
            if n == 0 {
                return Err(format!(
                    "`{}` closed while answering `{command}`",
                    self.label
                ));
            }
            let trimmed = line.trim_end();
            if !started {
                if let Some(rest) = trimmed.strip_prefix('=') {
                    started = true;
                    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
                    body.push_str(rest.trim());
                } else if let Some(rest) = trimmed.strip_prefix('?') {
                    return Err(format!("`{command}` refused: {}", rest.trim()));
                }
                continue;
            }
            if trimmed.is_empty() {
                return Ok(body.trim().to_string());
            }
            body.push('\n');
            body.push_str(trimmed);
        }
    }
}

impl Drop for GtpEngine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

/// One move an engine offered, and what it thought of it.
pub struct Suggestion {
    pub mv: Move,
    /// From the perspective of the side to play, in [0, 1].
    pub winrate: f64,
    pub visits: u64,
    /// The line the engine expects, starting with `mv`.
    pub pv: Vec<Move>,
}

/// What an engine thinks of a position, however it was asked.
pub struct Advice {
    pub best: Move,
    /// Most-visited first. Empty if the engine would only answer with a move.
    pub candidates: Vec<Suggestion>,
    pub source: String,
}

impl GtpEngine {
    /// Brings the engine up to `game`'s position from scratch. Replaying the
    /// moves is not enough — a position typed in by hand has stones but no
    /// history — so this uses `set_free_handicap` and `play` for what is on the
    /// board, which every engine understands.
    pub fn set_position(&mut self, board: &crate::board::Board, komi: f32) -> Result<(), String> {
        self.send(&format!("boardsize {}", board.size()))?;
        self.send("clear_board")?;
        self.send(&format!("komi {komi}"))?;
        for pt in board.points() {
            let colour = match board.cell(pt) {
                crate::board::Cell::Black => "b",
                crate::board::Cell::White => "w",
                _ => continue,
            };
            // `play` in alternation would reject these, but every engine
            // accepts an explicit colour, and none of them mind the order.
            self.send(&format!(
                "play {colour} {}",
                format_move(board, Move::Play(pt))
            ))?;
        }
        Ok(())
    }

    /// Asks for the best move, with the analysis behind it when the engine has
    /// one to give. `kata-analyze` and `lz-analyze` stream until they are
    /// interrupted, so this reads them for `millis` and then stops them.
    pub fn advise(
        &mut self,
        board: &crate::board::Board,
        komi: f32,
        millis: u64,
    ) -> Result<Advice, String> {
        self.set_position(board, komi)?;
        let colour = if board.to_move() == Color::Black {
            "b"
        } else {
            "w"
        };
        let commands = self.send("list_commands").unwrap_or_default();
        let analyse = if commands.lines().any(|l| l.trim() == "kata-analyze") {
            Some("kata-analyze")
        } else if commands.lines().any(|l| l.trim() == "lz-analyze") {
            Some("lz-analyze")
        } else {
            None
        };
        if let Some(verb) = analyse
            && let Ok(advice) = self.analyse(board, verb, colour, millis)
            && !advice.candidates.is_empty()
        {
            return Ok(advice);
        }
        // Anything that speaks GTP can at least be asked to move and take it
        // back again.
        let answer = self.send(&format!("genmove {colour}"))?;
        let best = parse_move(board, &answer)
            .ok_or_else(|| format!("`{}` answered `{answer}`", self.label))?;
        let _ = self.send("undo");
        Ok(Advice {
            best,
            candidates: Vec::new(),
            source: format!("{} (genmove only, no analysis)", self.label),
        })
    }

    fn analyse(
        &mut self,
        board: &crate::board::Board,
        verb: &str,
        colour: &str,
        millis: u64,
    ) -> Result<Advice, String> {
        writeln!(self.stdin, "{verb} {colour} 50").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(millis);
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            let mut line = String::new();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("engine closed mid-analysis".to_string());
            }
            if line.starts_with("info ") {
                last = line;
            }
        }
        // Any input stops the stream; then the response has to be drained or
        // the next command reads this one's leftovers.
        writeln!(self.stdin).map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        loop {
            let mut line = String::new();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
                break;
            }
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if t.starts_with("info ") {
                last = line;
            }
        }
        let candidates = parse_analysis(board, &last);
        Ok(Advice {
            best: candidates.first().map_or(Move::Pass, |c| c.mv),
            candidates,
            source: self.label.clone(),
        })
    }
}

/// Reads one `info ...` line of the Leela/KataGo analysis format: a run of
/// `move X visits N winrate F ... pv A B C` blocks, one per candidate.
fn parse_analysis(board: &crate::board::Board, line: &str) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for block in line.split("info ").skip(1) {
        let words: Vec<&str> = block.split_whitespace().collect();
        let mut mv = None;
        let mut visits = 0u64;
        let mut winrate = 0.0f64;
        let mut pv = Vec::new();
        let mut i = 0;
        while i < words.len() {
            match words[i] {
                "move" if i + 1 < words.len() => {
                    mv = parse_move(board, words[i + 1]);
                    i += 2;
                }
                "visits" if i + 1 < words.len() => {
                    visits = words[i + 1].parse().unwrap_or(0);
                    i += 2;
                }
                "winrate" if i + 1 < words.len() => {
                    winrate = words[i + 1].parse().unwrap_or(0.0);
                    i += 2;
                }
                "pv" => {
                    let mut probe = *board;
                    for w in &words[i + 1..] {
                        let Some(m) = parse_move(board, w) else { break };
                        if probe.play(m).is_err() {
                            break;
                        }
                        pv.push(m);
                    }
                    break;
                }
                _ => i += 1,
            }
        }
        if let Some(mv) = mv {
            out.push(Suggestion {
                mv,
                winrate,
                visits,
                pv,
            });
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.visits));
    out
}

pub struct MatchSettings {
    pub games: usize,
    pub size: usize,
    pub komi: f32,
    pub params: Params,
    pub verbose: bool,
    /// Handicap stones for gobot, which then always takes Black and the
    /// opponent moves first. This is how an engine gets measured against one
    /// far stronger than it: not by the win rate, which would be zero, but by
    /// the number of stones it takes to make the win rate a half.
    pub handicap: usize,
}

struct GameResult {
    score: f32,
    moves: usize,
    ended: &'static str,
}

/// Plays `settings.games` games, gobot taking Black and White by turns so that
/// komi does not decide the result on its own.
pub fn run_match(command: &str, settings: &MatchSettings) -> Result<(), String> {
    let mut engine = GtpEngine::spawn(command)?;
    println!(
        "gobot vs {} — {} games on {}x{}, komi {}, {} per move",
        engine.label,
        settings.games,
        settings.size,
        settings.size,
        settings.komi,
        budget(&settings.params)
    );

    if settings.handicap > 0 {
        println!(
            "  {} handicap stones to gobot, which takes Black throughout",
            settings.handicap
        );
    }
    let mut wins = 0usize;
    for g in 0..settings.games {
        let gobot_was = if settings.handicap > 0 || g % 2 == 0 {
            Color::Black
        } else {
            Color::White
        };
        let result = play_one(&mut engine, settings, gobot_was, g as u64)?;
        let won = match gobot_was {
            Color::Black => result.score > 0.0,
            Color::White => result.score < 0.0,
        };
        if won {
            wins += 1;
        }
        println!(
            "  game {:>3}: gobot {:<5} {}  ({}, {} moves)  — running {}/{}",
            g + 1,
            gobot_was.name(),
            if won { "WIN " } else { "loss" },
            result.ended,
            result.moves,
            wins,
            g + 1
        );
    }

    let n = settings.games as f64;
    let p = wins as f64 / n;
    // A plain binomial standard error: enough to say whether the number means
    // anything yet, and it usually does not at twenty games.
    let se = (p * (1.0 - p) / n).sqrt();
    println!(
        "\ngobot won {wins}/{} = {:.1}% (+/- {:.1} points, one standard error)",
        settings.games,
        p * 100.0,
        se * 100.0
    );
    if settings.games < 100 {
        println!(
            "At {} games that interval is too wide to rank anything. Run more.",
            settings.games
        );
    }
    Ok(())
}

fn play_one(
    engine: &mut GtpEngine,
    settings: &MatchSettings,
    gobot_was: Color,
    seed: u64,
) -> Result<GameResult, String> {
    engine.send(&format!("boardsize {}", settings.size))?;
    engine.send("clear_board")?;
    engine.send(&format!("komi {}", settings.komi))?;

    let mut game = Game::new(settings.size, settings.komi);
    if settings.handicap > 0 {
        // The opponent chooses the points and tells us where they went, so the
        // two boards cannot drift apart over a convention neither wrote down.
        let placed = engine.send(&format!("fixed_handicap {}", settings.handicap))?;
        let mut n = 0;
        for vertex in placed.split_whitespace() {
            let Some(Move::Play(pt)) = parse_move(game.board(), vertex) else {
                return Err(format!("handicap vertex `{vertex}` makes no sense"));
            };
            game.setup_stone(pt, Color::Black);
            n += 1;
        }
        if n != settings.handicap {
            return Err(format!(
                "asked for {} handicap stones and got {n}",
                settings.handicap
            ));
        }
        game.set_to_move(Color::White);
    }
    let limit = settings.size * settings.size * 3;
    let mut ended = "both passed";
    let mut moves = 0;

    while moves < limit {
        if game.board().passes() >= 2 {
            break;
        }
        let to_move = game.to_move();
        let colour_letter = if to_move == Color::Black { "b" } else { "w" };
        let mv = if to_move == gobot_was {
            let mut p = settings.params;
            p.komi = settings.komi;
            p.seed ^= seed.wrapping_mul(0x9E37_79B9) ^ moves as u64;
            let result = mcts::search(game.board(), &p);
            if result.winrate() < 0.08 && result.playouts > 5_000 {
                ended = "gobot resigned";
                return Ok(GameResult {
                    score: if gobot_was == Color::Black { -1.0 } else { 1.0 },
                    moves,
                    ended,
                });
            }
            let mv = result.best;
            engine.send(&format!(
                "play {colour_letter} {}",
                format_move(game.board(), mv)
            ))?;
            mv
        } else {
            let answer = engine.send(&format!("genmove {colour_letter}"))?;
            if answer.eq_ignore_ascii_case("resign") {
                ended = "opponent resigned";
                return Ok(GameResult {
                    score: if gobot_was == Color::Black { 1.0 } else { -1.0 },
                    moves,
                    ended,
                });
            }
            parse_move(game.board(), &answer)
                .ok_or_else(|| format!("`{}` answered `{answer}`", engine.label))?
        };

        if game.play(mv).is_err() {
            // Our rules refused a move the opponent believes in. Passing keeps
            // the game going, but it means the two are not playing the same
            // rules and the result is not worth much.
            eprintln!(
                "  warning: {} is illegal here, passing instead",
                format_move(game.board(), mv)
            );
            let _ = game.play(Move::Pass);
        }
        moves += 1;
        if settings.verbose {
            print!("{}", crate::coords::diagram(game.board(), &[]));
        }
    }
    if moves >= limit {
        ended = "move limit";
    }
    Ok(GameResult {
        score: game.score(),
        moves,
        ended,
    })
}

fn budget(params: &Params) -> String {
    match (params.time_limit, params.playouts) {
        (Some(d), _) => format!("{:.1}s", d.as_secs_f64()),
        (None, Some(n)) => format!("{n} playouts"),
        _ => "nothing".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Board;

    /// A real `kata-analyze` line, captured from KataGo 1.18.2 on a 9x9 board
    /// after `play b e5` and `play w d4`. Invented test data would have agreed
    /// with whatever the parser happened to do.
    const REAL: &str = "info move E4 visits 828 edgeVisits 828 utility -0.49671 winrate 0.26204 scoreMean -0.475789 scoreStdev 8.18152 scoreLead -0.475789 scoreSelfplay -0.898867 prior 0.598353 lcb 0.24497 utilityLcb -0.544504 weight 686.82 order 0 pv E4 D3 D5 F2 G3 F7 E8 G5 G4 E7 E3 E2 F8 G8 info move D5 visits 828 edgeVisits 828 utility -0.49671 winrate 0.26204 scoreMean -0.475789 scoreStdev 8.18152 scoreLead -0.475789 scoreSelfplay -0.898867 prior 0.598353 lcb 0.24497 utilityLcb -0.544504 weight 686.82 isSymmetryOf E4 order 1 pv D5 C4 E4 B6 C7 G6 H5 E7 D7 G5 C5 B5 H6 H7 info move F3 visits 1 edgeVisits 1 utility -0.837767 winrate 0.0959983 scoreMean -0.545739 scoreStdev 5.24073 scoreLead -0.545739 scoreSelfplay -1.71156 prior 0.00405239 lcb -1.154 utilityLcb -4.33777 weight 2.12616 order 2 pv F3 ";

    #[test]
    fn it_reads_katagos_analysis_line() {
        let board = Board::new(9);
        let got = parse_analysis(&board, REAL);
        assert_eq!(got.len(), 3, "one entry per `info` block");
        let first = &got[0];
        assert_eq!(format_move(&board, first.mv), "E4");
        assert_eq!(first.visits, 828, "`visits`, not `edgeVisits` or `order`");
        assert!(
            (0.26..0.27).contains(&first.winrate),
            "winrate should be 0.262, got {}",
            first.winrate
        );
        assert!(
            first.pv.len() > 3,
            "the principal variation should come through"
        );
        assert_eq!(format_move(&board, first.pv[0]), "E4");
        // The third block is a move the search barely looked at.
        assert_eq!(got[2].visits, 1);
    }
}
