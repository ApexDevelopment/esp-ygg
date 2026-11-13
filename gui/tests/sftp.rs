//! SFTP's questions, answered on the graphical interface.

use std::rc::Rc;
use std::time::{Duration, Instant};

use esp_ygg_app::Settings;
use esp_ygg_app::sftp::Status;
use esp_ygg_app::ui::{Command, Key};
use esp_ygg_gui::Gui;
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};

struct Headless {
	window: Rc<MinimalSoftwareWindow>,
	started: Instant,
}

impl Platform for Headless {
	fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
		Ok(self.window.clone())
	}

	fn duration_since_start(&self) -> Duration {
		self.started.elapsed()
	}
}

fn sftp_commands(gui: &mut Gui) -> Vec<Command> {
	std::iter::from_fn(|| gui.poll_command())
		.filter(|command| matches!(command, Command::SftpPassword(_) | Command::SftpAdmit(_)))
		.collect()
}

#[test]
fn answers_a_password_once_and_only_with_it() {
	let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
	slint::platform::set_platform(Box::new(Headless {
		window,
		started: Instant::now(),
	}))
	.expect("setting the platform");
	let mut gui = Gui::new(Settings::new()).expect("creating the interface");
	gui.window().show().expect("showing the window");

	let asked = Status {
		password_for: Some("sftp".into()),
		..Status::default()
	};
	gui.set_sftp_status(asked.clone());
	gui.tick();
	for character in "sesame".chars() {
		gui.type_key(Key::Char(character));
	}
	gui.type_key(Key::Enter);
	gui.tick();
	assert_eq!(
		sftp_commands(&mut gui),
		[Command::SftpPassword(Some("sesame".into()))]
	);

	// Asked again, and given up on
	gui.set_sftp_status(Status::default());
	gui.set_sftp_status(asked);
	gui.tick();
	gui.type_key(Key::Escape);
	gui.tick();
	assert_eq!(sftp_commands(&mut gui), [Command::SftpPassword(None)]);
}
