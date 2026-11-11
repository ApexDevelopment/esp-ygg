//! The speaker icon in the title bar, drawn in pixels over two blank cells
//! the interface leaves for it, as no font has these glyphs.

use core::sync::atomic::{AtomicU8, Ordering};

use esp_ygg_app::ui::{SPEAKER_FROM_RIGHT, Speaker};
use mousefood::ColorTheme;
use mousefood::embedded_graphics::pixelcolor::{Rgb565, RgbColor};
use mousefood::embedded_graphics::prelude::*;
use mousefood::embedded_graphics::primitives::Rectangle;

const WIDTH: usize = 12;
const HEIGHT: usize = 10;
type Icon = [&'static str; HEIGHT];

const ON: Icon = [
	"............",
	"....#...#...",
	"...##....#..",
	"#####.#...#.",
	"#####..#..#.",
	"#####..#..#.",
	"#####.#...#.",
	"...##....#..",
	"....#...#...",
	"............",
];

const SILENT: Icon = [
	".#..........",
	"..#.#.......",
	"...#........",
	"###.#.......",
	"####.#......",
	"#####.#.....",
	"#####..#....",
	"...##...#...",
	"....#....#..",
	"..........#.",
];

const MUTED: Icon = [
	"............",
	"....#...##..",
	"...##...##..",
	"#####...##..",
	"#####...##..",
	"#####...##..",
	"#####.......",
	"...##...##..",
	"....#...##..",
	"............",
];

static SHOWN: AtomicU8 = AtomicU8::new(Speaker::On as u8);

pub fn show(speaker: Speaker) {
	SHOWN.store(speaker as u8, Ordering::Relaxed);
}

/// Draws the icon. Called after every flush, as the terminal may have
/// drawn its blank cells over it.
pub fn draw<D: DrawTarget<Color = Rgb565>>(display: &mut D, cell: Size) {
	let icon = match SHOWN.load(Ordering::Relaxed) {
		value if value == Speaker::Silent as u8 => &SILENT,
		value if value == Speaker::Muted as u8 => &MUTED,
		_ => &ON,
	};
	let columns = display.bounding_box().size.width / cell.width;
	let left = (columns - u32::from(SPEAKER_FROM_RIGHT)) * cell.width;
	let area = Rectangle::new(
		Point::new(left as i32, 0),
		Size::new(WIDTH as u32, HEIGHT as u32),
	);
	let background = Rgb565::from(ColorTheme::default().green);
	let pixels = icon.iter().flat_map(|row| {
		row.bytes().map(move |pixel| {
			if pixel == b'#' {
				Rgb565::BLACK
			} else {
				background
			}
		})
	});
	let _ = display.fill_contiguous(&area, pixels);
}
