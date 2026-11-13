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
}
