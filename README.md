# gobot

A Go engine for 9x9, built to be a second opinion on a game you are playing
somewhere else. You type in the position, you type in your opponent's moves, and
it tells you what it would play and how it rates the position.

Rust, no dependencies, one binary.

```sh
cargo build --release
./target/release/gobot serve      # a board in a browser  <- start here
./target/release/gobot            # the same thing in the terminal
./target/release/gobot help
```

## The board in a browser

```sh
gobot serve
gobot serve --komi 6.5 --engine "katago gtp -model $NET -override-config maxVisits=3000"
```

Then open `http://127.0.0.1:8080`. The page has two modes, and they are
different things:

- **Set up** — you place every stone yourself, both colours, and say whose turn
  it is. Nothing moves on its own. This is the mode for following a game being
  played somewhere else: enter the position, enter your opponent's reply as it
  comes, and ask what to play.
- **Play** — you take one colour and the engine takes the other. Clicking an
  empty point plays your move and the engine answers it.

`Analyse` works in both, always for the side to play. Suggestions are drawn on
the board as discs sized by how hard the engine looked at each — which is what
ranks them — with the win rate as the number inside; tapping one in the list
plays that variation out as numbered stones. Board size, komi, thinking time, handicap, undo and pass are
all on the page.

A turn in Play mode is two round trips, and the split is the point: the first
plays your move under the rules and hands the board straight back, so the stone
and anything it captures appear the moment you tap; the second asks the engine
for an answer and plays that. Doing both in one request left the board
unchanged for however long the engine thought, and a tap that shows nothing
reads as a tap that missed. Captures on either move land on the page without it
knowing a single rule of Go.

`--bind` is the point of it being a web page: the board can be open on the phone
or tablet you are playing on. On startup it prints the addresses this machine
answers on, tailnet addresses first.

```sh
gobot serve --bind 100.x.y.z    # this machine's tailnet address: only that tailnet
gobot serve --bind 0.0.0.0      # every network this machine is on
```

Binding to a Tailscale address is the narrower of the two and usually the
easier: the phone is already on the tailnet, and nothing else can see the
board. `0.0.0.0` opens it to whatever network the machine is on, so stop it when
you are done.

The page is laid out for a phone as well as a desktop — board above the
controls, 44-pixel tap targets, and Analyse pinned to the bottom of the screen
since it is the button pressed every turn. Where an intersection comes out
narrower than a fingertip, which is a 13x13 or 19x19 board on a phone, the
first tap aims and the second commits.

The server is `TcpListener` and about five hundred lines, because one page and
three endpoints do not justify a dependency. The page posts a few lines of
`key value` and gets a few lines back, one of which is the board as the rules
resolve it — so a stone that cannot exist is taken off *and named* on screen,
rather than silently changing which position is being analysed.

It keeps nothing between requests: every one rebuilds the board from the stones
the page sends. A position has no memory of the capture that made it, so the ko
point and the positions the game has already stood in ride along with each
request as well — without them a ko can be retaken, and the engine will happily
suggest it. Superko over the web is therefore a rule the server holds an honest
page to, not one it can enforce against a client that lies about its own
history.

## The terminal board

```
$ gobot --time 5
gobot 0.1.0 — 9x9, komi 7.5, hints for black, 5.0s per hint

black (you) > setup
Paste 9 rows, top row first. X or # is Black, O is White, . is empty.
. . . . . . . . .
. . . O . . . . .
...

black (you) > hint
  A B C D E F G H J
9 . . . . . . . . . 9
6 . . O . .(*)X . . 6
...
black to play — 215,458 playouts in 2.0s on 8 thread(s)
  -> F6    44.7%      4,759 visits   F6 D4
     D5    44.4%      4,680 visits   D5 F5
```

`setup` takes the position as a diagram, which is the quickest way to get a
board off another screen and into here. After that, `w e5` enters your
opponent's move and the next hint prints by itself. `help` lists the rest.

### Board size and stones already on it

`--size` is any board from 2x2 to 19x19; everything else follows it, including
the star points, the handicap points and the column letters.

There are three ways to put stones down before play starts, and they compose:

| | |
|---|---|
| `--handicap <n>` or `handicap <n>` | the conventional 2-9 stones for Black, White to move |
| `setup` | the whole position as a diagram, `<n>` rows pasted top-first |
| `add b\|w <vertex>...` and `del <vertex>` | one point at a time |

```sh
gobot --size 13 --handicap 4 --komi 0.5      # a 13x13 four-stone game
gobot --size 19 --diagram board.txt          # a position from a file
```

A stone with no liberties cannot be on a board, so one typed into a diagram gets
left off — and the board says which points those were, because otherwise a
single mis-copied point quietly turns it into a different position.

A position is handed to a `--engine` opponent as one `play` per stone with the
colour named, since a position typed in by hand has no move history to replay
instead. `tests/rules.rs` checks that this reproduces the position exactly over
random boards, which is the assumption the whole `--engine` path rests on.

### Hints from a stronger engine

`--engine` keeps this board and this way of entering a position, but asks
something else what to play. KataGo is the obvious thing to point it at — its
code is MIT, the networks it ships are CC0, and it needs setting up once:

```sh
brew install katago
mkdir -p ~/.katago
cp "$(brew --prefix katago)"/share/katago/configs/gtp_example.cfg ~/.katago/default_gtp.cfg
```

That last line is what lets `katago gtp` run without being handed a `-config`
every time. KataGo also writes a log per session into `gtp_logs/` under whatever
directory it was started from, which `.gitignore` covers; add
`logDir=,logToStderr=false` to the overrides to stop it. Then pick one of the networks the formula ships:

```sh
NET=$(ls "$(brew --prefix katago)"/share/katago/kata1-*.bin.gz | head -1)
gobot --time 4 --engine \
  "katago gtp -model $NET -override-config maxVisits=2000,numSearchThreads=4,rules=tromp-taylor"
```

On a 9x9 board that is not the net to use. KataGo's main run trains on 19x19,
and there is a [separately finetuned 9x9
net](https://katagotraining.org/extra_networks/) trained on nothing else. It is
the same architecture and size as the general net of its generation, so it
costs nothing: in 60 games between the two at 400 visits a move, 9x9, it won 30
of the 36 that were decided — the other 24 were draws, because that match used
an integer komi and an integer komi on a 9x9 area-scored board lands on jigo
constantly. It was also the faster of the two, 0.50 against 0.54 seconds a
move.

```sh
curl -O https://media.katagotraining.org/uploaded/networks/models_extra/kata9x9-b18c384nbt-20231025.bin.gz
gobot serve --engine \
  "katago gtp -model kata9x9-b18c384nbt-20231025.bin.gz -override-config numSearchThreads=20,maxVisits=20000,rules=tromp-taylor"
```

`numSearchThreads` is worth measuring rather than guessing: the config KataGo
ships sets 6, and on the machine above 20 was the peak at 4,628 visits in a
five-second budget against 3,037 for 6 — with 32 and beyond falling off a
cliff. `maxVisits` wants to be high enough never to bind, because the server
waits out its whole time budget either way; the shipped config sets 500, which
a five-second budget passes in under two.

The position is rebuilt on the other engine stone by stone rather than replayed
as a game, because a position typed in by hand has stones but no history.

### Any engine, not that one

KataGo is what these examples use and nothing here knows it exists. `--engine`
and `--against` take a command line, and whatever is on the other end is spoken
to in GTP. To ask for a move it needs:

| | |
|---|---|
| `boardsize`, `clear_board`, `komi`, `play`, `genmove` | required |
| `undo` | so a suggestion can be taken back rather than played |
| `list_commands`, `name` | how the analysis support below is found, and what the page labels it |
| `fixed_handicap` | only for `gobot match --handicap` |
| `kata-analyze` or `lz-analyze` | optional |

With one of the analyse commands the win rates, visit counts and variations come
from the engine. Without, it is asked to `genmove` and then `undo`, which yields
a move and nothing else — the board still shows it, the table is just empty.
Leela Zero, Sai, and anything else built on that lineage answer `lz-analyze`;
GNU Go and older engines answer neither and take the plain path.

gobot itself speaks GTP, so it can be the engine behind its own board, which is
the short way to see that the slot is not KataGo-shaped:

```sh
gobot serve --engine "gobot gtp --playouts 20000"
gobot match --games 2 --handicap 4 --komi 0.5 --against "gobot gtp --playouts 400"
```


The engine also speaks GTP (`gobot gtp`), which is how it gets plugged into
Sabaki or GoGui.

## Measuring it

`gobot match` drives another GTP engine and counts the games, because "is it
stronger?" has no other honest answer.

```sh
gobot match --games 20 --time 1 --against "<a command that speaks GTP>"
gobot match --games 20 --time 1 --komi 0.5 --handicap 4 --against "..."
```

Against an opponent far out of reach the win rate is zero and says nothing, so
`--handicap` asks the other question instead: how many stones does it take to
make the game even? The opponent places them with `fixed_handicap` and tells us
where, so the two boards cannot drift apart over a convention neither of them
wrote down.

Two notes on picking an opponent, both learned here:

- **GNU Go 3.8 is unusable on this machine.** The Homebrew build asserts and
  dies inside `genmove` on an empty board (`board.c:2540 - ON_BOARD1(str) near
  PASS`), with or without flags. It would otherwise be the natural yardstick.
- **KataGo works, and can be turned down.** With `maxVisits=1` it plays its
  policy network straight out, which is fast and still far above this engine.

```sh
NET=$(ls "$(brew --prefix katago)"/share/katago/g170e-*.bin.gz | head -1)
gobot match --games 6 --time 1 --komi 0.5 --handicap 4 --against \
  "katago gtp -model $NET -override-config maxVisits=1,numSearchThreads=1,rules=tromp-taylor"
```

`rules=tromp-taylor` matters: this engine scores by area with every dead stone
actually captured, so an opponent that passes over dead stones would be scored
wrong. Setting both sides to the same rules is what keeps the count honest.

## What is inside

| | |
|---|---|
| `src/board.rs` | the rules: chains, captures, simple ko, eyes, area scoring, Zobrist |
| `src/game.rs` | positional superko and undo, which the search does not need |
| `src/mcts.rs` | UCT with RAVE, root-parallel, and the playout policy |
| `src/coords.rs` | vertex names and diagrams in and out |
| `src/gtp.rs` | GTP, enough of it to be driven by a GUI or a harness |
| `src/arena.rs` | plays a series against another GTP engine and counts it |
| `src/web.rs`, `src/web/index.html` | the local server and the one page it serves |

## Measured, on an M3 Pro (14 cores)

| | |
|---|---|
| playouts | 15,400/s on one thread, 111,000/s on eight |
| a 2-second hint | about 215,000 playouts on a mid-game 9x9 board |

Against KataGo playing its `g170e-b20c256x2` policy network straight out
(`maxVisits=1`), on 9x9, gobot thinking one second a move:

| handicap to gobot | games won | |
|---|---|---|
| 2 stones | 0 of 6 | 0% |
| 3 stones | 5 of 12 | 42% +/- 14 |
| 4 stones | 5 of 6 | 83% |
| 6 stones | 6 of 6 | 100% |

Three stones is where it is roughly level — 42% over twelve games, with a
standard error wide enough that an even match sits inside it. That is the only
statement about this engine's strength that has been earned. It is a
comparison and not a rank: turning it into one needs an opponent whose own rank
is known, and twelve games would not be enough for that either.

Two things that series showed and that are worth knowing before trusting a hint:

- **At six stones the games ended after two moves.** Winning already, the search
  passes, and under area scoring that is not wrong — but it is the same
  indifference that makes its advice worthless once a game is decided.
- **Its evaluation can be far out.** In one mid-game position gobot called it
  44% for Black while KataGo called the same position 0.08%, fifteen points
  behind. On an open board the two broadly agree; where they do not, this engine
  is the one that is wrong.

`gobot bench` reproduces the first line. The eight-thread figure is root
parallelisation — eight independent trees whose root statistics are summed — so
it buys breadth, not depth, and eight times the playouts is worth appreciably
less than eight times the thinking time.

## What this does not tell you

- **Still no rank, but now for a better reason.** Turning a win rate into a kyu
  rank needs an opponent whose own rank is known, and KataGo's human-imitation
  network is one — set `humanSLProfile` and it plays like a human of that rank.
  Against it on 9x9, at a second a move, gobot is exactly even with `rank_15k`
  (18 of 36) and clearly below `rank_10k` (2 of 12). That still does not make
  gobot 15 kyu. The scale does not separate its weak end on this board size —
  `rank_20k`, five ranks weaker, only loses 7 of 12 — so the measurement is
  bracketed from above and not from below. And the model learned rank from
  human games, which are overwhelmingly 19x19: what separates 20k from 15k
  there is opening shape that a 9x9 board barely has room for. The honest
  reading is that gobot is somewhere below 10 kyu, and that this ladder cannot
  say where.
- **No neural network.** Playouts are random with two filters (no filling your
  own eyes, no walking into atari) plus an atari reply. This is the pre-AlphaGo
  design, and its ceiling is well below any engine with a policy net.
- **Win rate, not margin.** The search maximises the chance of winning, so in a
  position that is already decided it will report every move at the same value
  and pick among them more or less arbitrarily. Two of the tests in
  `tests/search.rs` exist because that behaviour was first mistaken for a bug.
- **The tree is shallow.** Principal variations come out two or three moves
  long. `expand_threshold` in `Params` is the dial, and it has not been tuned
  against anything.

## Tests

```sh
cargo test --release
```

`tests/rules.rs` covers the rules; every case is written so that it would fail
if the rule it names were simply missing, rather than passing on an engine with
no rules at all. `tests/search.rs` covers the search: it beats random play, more
playouts beat fewer, it never suggests an illegal move, and its win rate points
the right way in a decided position.

One defect that the tests caught and that is worth knowing about: the playout
policy originally answered only ataris created by the last move played. A
position handed to the engine — a handicap setup, or a game joined halfway —
can already contain an atari that no recent move created, and the engine was
blind to it. `mcts::atari_reply` is the fix and has its own tests.
