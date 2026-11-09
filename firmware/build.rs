const SETTINGS: [&str; 7] = [
	"ESP_YGG_CHANNEL",
	"ESP_YGG_NAME",
	"ESP_YGG_PASSWORD",
	"ESP_YGG_PEER",
	"ESP_YGG_STATUS_PAGE",
	"ESP_YGG_WIFI_SSID",
	"ESP_YGG_WIFI_PASSWORD",
];

fn main() {
	for setting in SETTINGS {
		println!("cargo:rerun-if-env-changed={setting}");
	}
	// A floor for the node's clock, which has no real-time source
	let now = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|elapsed| elapsed.as_secs())
		.unwrap_or(0);
	println!("cargo:rustc-env=ESP_YGG_BUILD_TIME={now}");
	check_xtensa_linker_available();
	// make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
	println!("cargo:rustc-link-arg=-Tlinkall.x");
}

#[cfg(unix)]
fn check_xtensa_linker_available() {
	println!("cargo:rerun-if-env-changed=PATH");

	let target = std::env::var("TARGET").unwrap_or_default();
	println!(
		"cargo:rerun-if-env-changed={}",
		cargo_linker_env_var(&target)
	);

	let linker = xtensa_linker(&target);

	let error = match std::process::Command::new(&linker)
		.arg("--version")
		.output()
	{
		Ok(_) => return,
		Err(error) => error,
	};

	let export_file = std::env::var("HOME")
		.map(|home| format!("{home}/export-esp.sh"))
		.unwrap_or_else(|_| "$HOME/export-esp.sh".to_string());

	panic!(
		"Xtensa linker `{linker}` was not found in PATH or could not be executed: {error}.\n\n\
         Xtensa targets on Unix require espup's environment export file to be sourced before building.\n\
         Try: `source {export_file}`\n\n\
         If the export file does not exist, run `espup install` first.\n\
         For more details, see:\n\
         https://github.com/esp-rs/espup?tab=readme-ov-file#environment-variables-setup"
	);
}

#[cfg(not(unix))]
fn check_xtensa_linker_available() {}

#[cfg(unix)]
fn xtensa_linker(target: &str) -> String {
	if let Some(linker) = cargo_configured_linker(target) {
		return linker;
	}

	target
		.strip_prefix("xtensa-")
		.and_then(|target| target.strip_suffix("-none-elf"))
		.map(|chip| format!("xtensa-{chip}-elf-gcc"))
		.unwrap_or_else(|| "xtensa-esp32-elf-gcc".to_string())
}

#[cfg(unix)]
fn cargo_configured_linker(target: &str) -> Option<String> {
	std::env::var(cargo_linker_env_var(target)).ok()
}

#[cfg(unix)]
fn cargo_linker_env_var(target: &str) -> String {
	let target = target.replace('-', "_").to_ascii_uppercase();
	format!("CARGO_TARGET_{target}_LINKER")
}
