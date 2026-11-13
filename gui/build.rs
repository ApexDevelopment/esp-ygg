use std::collections::HashMap;
use std::path::Path;

/// Text sizes to draw glyphs at ahead of time. Material sets its sizes in
/// a way Slint cannot see when building, so they are listed here: those of
/// Material's text styles small enough for a 320x240 screen.
const FONT_SIZES: &str = "11,12,14,16,22,24";

fn main() {
	// SAFETY: nothing else runs in the build script yet to read the
	// environment while it changes.
	unsafe { std::env::set_var("SLINT_FONT_SIZES", FONT_SIZES) };
	// The folder rather than its main file, so the interface can also use
	// parts Material does not export, such as its navigation items.
	let material = Path::new(env!("CARGO_MANIFEST_DIR")).join("material-1.1.0");
	// Microcontrollers have no font files or image decoders, so glyphs and
	// images are drawn ahead of time and built in.
	let config = slint_build::CompilerConfiguration::new()
		.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
		.with_library_paths(HashMap::from([("material".into(), material)]));
	slint_build::compile_with_config("ui/main.slint", config).expect("compiling the interface");
	#[cfg(feature = "gopher")]
	write_advances();
}

/// Writes how far each printable ASCII character moves the pen in body
/// text, in 64ths of a pixel, for wrapping text to the screen in Rust.
#[cfg(feature = "gopher")]
fn write_advances() {
	const FONT: &str = "fonts/Roboto-Regular.ttf";
	/// Material's body_medium text.
	const SIZE: f32 = 14.0;
	println!("cargo::rerun-if-changed={FONT}");
	let data = std::fs::read(FONT).expect("reading the font");
	let face = ttf_parser::Face::parse(&data, 0).expect("parsing the font");
	let scale = SIZE * 64.0 / f32::from(face.units_per_em());
	let advances: Vec<String> = (' '..='~')
		.map(|character| {
			let glyph = face.glyph_index(character).expect("a glyph for ASCII");
			let advance = face.glyph_hor_advance(glyph).unwrap_or_default();
			((f32::from(advance) * scale).round() as u32).to_string()
		})
		.collect();
	let code = format!(
		"/// Advances of ' ' to '~' in body text, in 64ths of a pixel.\npub const ADVANCES: [u32; 95] = [{}];\n",
		advances.join(", ")
	);
	let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("advances.rs");
	std::fs::write(out, code).expect("writing the advances");
}
