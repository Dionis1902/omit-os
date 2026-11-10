// SPDX-License-Identifier: GPL-2.0-only
//! `pop` — a small game: click the discs before they shrink away.
//!
//! Discs appear at random places and shrink; a click on one scores a
//! point, a disc that vanishes costs a life. Three lives; `q` quits.
//! `pop --test` makes the discs appear at fixed places and last long,
//! for the automated test.
//!
//! The game redraws at a fixed rate, which makes it the system's first
//! program whose *frame pacing* matters: on exit it reports how evenly
//! its frames were spaced.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use omit_gfx::geometry::Rect;
use omit_gfx::surface::Surface;
use omit_rt::window::{Event, Window};
use omit_rt::{eprintln, println};

omit_rt::entry!(main);

const WIDTH: usize = 400;
const HEIGHT: usize = 300;
/// Height of the strip with the score.
const HEADER: i32 = 24;
const BACKGROUND: u32 = 0x12_1A_2B;
const HEADER_COLOUR: u32 = 0x1E_2A_44;
const TEXT: u32 = 0xF0_F4_FF;
const COLOURS: [u32; 4] = [0xE8_5D_4A, 0x4A_B8_E8, 0x6B_D4_6B, 0xE8_C8_4A];
/// Time between frames, in microseconds: 50 frames per second, the
/// fastest rate the system's 10 ms timer can pace evenly.
const FRAME_US: u64 = 20_000;
const START_RADIUS: i32 = 34;
const LIVES: u32 = 3;

struct Disc {
    x: i32,
    y: i32,
    /// In sixteenths of a pixel, so that slow shrinking is smooth.
    radius: i32,
    colour: u32,
}

/// A small pseudo-random generator (xorshift); good enough for a game.
struct Random(u64);

impl Random {
    fn next(&mut self, bound: u32) -> i32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 32) as u32 % bound) as i32
    }
}

/// Where discs appear in test mode, in order.
const TEST_PLACES: [(i32, i32); 3] = [(100, 120), (280, 200), (200, 90)];

struct Game {
    discs: Vec<Disc>,
    score: u32,
    lives: u32,
    spawned: u32,
    /// Frames until the next disc appears.
    countdown: u32,
    random: Random,
    test: bool,
}

impl Game {
    fn spawn(&mut self) {
        let (x, y) = if self.test {
            TEST_PLACES[self.spawned as usize % TEST_PLACES.len()]
        } else {
            (
                START_RADIUS + self.random.next(WIDTH as u32 - 2 * START_RADIUS as u32),
                HEADER
                    + START_RADIUS
                    + self
                        .random
                        .next((HEIGHT as i32 - HEADER) as u32 - 2 * START_RADIUS as u32),
            )
        };
        self.discs.push(Disc {
            x,
            y,
            radius: START_RADIUS * 16,
            colour: COLOURS[self.spawned as usize % COLOURS.len()],
        });
        self.spawned += 1;
    }

    /// Advances the game by one frame.
    fn step(&mut self) {
        // Discs shrink faster as the score grows; not at all in test
        // mode.
        let shrink = if self.test {
            0
        } else {
            6 + self.score as i32 / 2
        };
        for disc in &mut self.discs {
            disc.radius -= shrink;
        }
        let before = self.discs.len();
        self.discs.retain(|disc| disc.radius > 4 * 16);
        self.lives = self
            .lives
            .saturating_sub((before - self.discs.len()) as u32);
        if self.countdown == 0 {
            // One disc at a time in test mode; otherwise more and more.
            if !self.test || self.discs.is_empty() {
                self.spawn();
            }
            self.countdown = if self.test {
                10
            } else {
                50u32.saturating_sub(self.score).max(15)
            };
        } else {
            self.countdown -= 1;
        }
    }

    /// A click: the topmost disc under the pointer pops.
    fn click(&mut self, x: i32, y: i32) {
        let hit = self.discs.iter().rposition(|disc| {
            let (dx, dy, radius) = (x - disc.x, y - disc.y, disc.radius / 16);
            dx * dx + dy * dy <= radius * radius
        });
        if let Some(index) = hit {
            self.discs.remove(index);
            self.score += 1;
        }
    }

    fn draw(&self, image: &mut Surface) {
        let mut canvas = image.canvas();
        canvas.fill(Rect::new(0, 0, WIDTH as i32, HEIGHT as i32), BACKGROUND);
        for disc in &self.discs {
            let radius = disc.radius / 16;
            // A filled circle, one horizontal line at a time.
            for dy in -radius..=radius {
                let half = isqrt(radius * radius - dy * dy);
                canvas.fill(
                    Rect::new(disc.x - half, disc.y + dy, 2 * half + 1, 1),
                    disc.colour,
                );
            }
        }
        canvas.fill(Rect::new(0, 0, WIDTH as i32, HEADER), HEADER_COLOUR);
        let status = if self.lives == 0 {
            format!("score: {}   game over - press n for a new game", self.score)
        } else {
            format!("score: {}   lives: {}", self.score, self.lives)
        };
        canvas.text(8, 4, &status, TEXT);
    }
}

/// The integer square root, rounded down.
fn isqrt(value: i32) -> i32 {
    let mut root = 0;
    while (root + 1) * (root + 1) <= value {
        root += 1;
    }
    root
}

fn new_game(test: bool) -> Game {
    Game {
        discs: Vec::new(),
        score: 0,
        lives: LIVES,
        spawned: 0,
        countdown: 0,
        random: Random(omit_rt::clock_us() | 1),
        test,
    }
}

fn main() -> i32 {
    let test = omit_rt::args().nth(1) == Some("--test");
    let mut window = match Window::open("Pop", WIDTH, HEIGHT) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("pop: cannot open a window: {error}");
            return 1;
        }
    };
    let mut image = Surface::new(WIDTH, HEIGHT, BACKGROUND);
    let mut game = new_game(test);

    // Frame pacing: each frame is due a fixed time after the previous
    // one was due (not after it was shown, which would let delays add
    // up). What is measured is when frames are actually shown.
    let start = omit_rt::clock_us();
    let mut due = start;
    let mut last_shown = start;
    let (mut frames, mut late, mut worst) = (0u64, 0u64, 0u64);
    // Whether the left button was down at the last pointer event: only
    // the press counts as a click, not the movement while it is held.
    let mut held = false;
    'game: loop {
        if game.lives > 0 {
            game.step();
        }
        game.draw(&mut image);
        window.pixels().copy_from_slice(image.pixels());
        window.present();
        let shown = omit_rt::clock_us();
        if frames > 0 {
            let interval = shown - last_shown;
            worst = worst.max(interval);
            // More than half a frame over the intended interval.
            late += u64::from(interval > FRAME_US * 3 / 2);
        }
        last_shown = shown;
        frames += 1;

        // Handle input until the next frame is due.
        due += FRAME_US;
        loop {
            let now = omit_rt::clock_us();
            if now >= due {
                // If the program fell far behind (it was not scheduled
                // for a while), do not try to catch up frame by frame.
                if now - due > 5 * FRAME_US {
                    due = now;
                }
                break;
            }
            match window.event(Some((due - now).div_ceil(1000))) {
                Some(Event::Pointer { x, y, buttons }) => {
                    let down = buttons & 1 != 0;
                    if down && !held {
                        game.click(x, y);
                    }
                    held = down;
                }
                Some(Event::Key(code)) => match u8::try_from(code).map(char::from) {
                    Ok('q') => break 'game,
                    Ok('n') => game = new_game(test),
                    _ => {}
                },
                Some(Event::Closed) => break 'game,
                _ => {}
            }
        }
    }
    let elapsed = omit_rt::clock_us() - start;
    println!(
        "pop: score {}; {} frames in {}.{} s, average interval {}.{} ms (intended {}), longest {}.{} ms, {} late",
        game.score,
        frames,
        elapsed / 1_000_000,
        (elapsed % 1_000_000) / 100_000,
        elapsed / frames.max(1) / 1000,
        (elapsed / frames.max(1) % 1000) / 100,
        FRAME_US / 1000,
        worst / 1000,
        (worst % 1000) / 100,
        late
    );
    0
}
