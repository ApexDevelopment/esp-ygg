use std::collections::HashMap;
use std::path::Path;

fn main() {
	let material = Path::new(env!("CARGO_MANIFEST_DIR")).join("material-1.1.0/material.slint");
	// Microcontrollers have no font files or image decoders, so glyphs and
	// images are drawn ahead of time and built in.
	let config = slint_build::CompilerConfiguration::new()
		.embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer)
		.with_library_paths(HashMap::from([("material".into(), material)]));
	slint_build::compile_with_config("ui/main.slint", config).expect("compiling the interface");
}
