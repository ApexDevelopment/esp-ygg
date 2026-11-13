//! The SFTP app: the shared folder's files, sending them, and asking the
//! person what SFTP needs to know.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use esp_ygg_app::sftp::{Entry, Status, size_text};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::front::sync;
use crate::{MainWindow, SftpFile};

pub struct SftpApp {
	window: MainWindow,
	files: Rc<VecModel<SftpFile>>,
	status: Status,
	/// Who to log in as on servers, which boards ignore.
	pub user: String,
}

impl SftpApp {
	pub fn new(window: &MainWindow) -> Self {
		let files = Rc::new(VecModel::default());
		window
			.global::<crate::Sftp>()
			.set_files(ModelRc::from(files.clone()));
		SftpApp {
			window: window.clone_strong(),
			files,
			status: Status::default(),
			user: "sftp".into(),
		}
	}

	pub fn set_files(&self, mut entries: Vec<Entry>) {
		entries.sort_by_key(|entry| entry.name.to_lowercase());
		let files = entries
			.into_iter()
			.map(|entry| SftpFile {
				name: entry.name.into(),
				size: size_text(entry.size).into(),
			})
			.collect();
		sync(&self.files, files);
	}

	/// Shows what SFTP is doing. Returns whether the folder is worth
	/// listing again.
	pub fn set_status(
		&mut self,
		status: Status,
		label: impl Fn(&core::net::Ipv6Addr) -> String,
	) -> bool {
		let sftp = self.window.global::<crate::Sftp>();
		let asking = status.asking.as_ref().map(&label).unwrap_or_default();
		sftp.set_asking(asking.into());
		sftp.set_status(status.summary().into());
		let ended = status.ended_since(&self.status);
		self.status = status;
		ended
	}

	/// The user a server wants a password for, if one does and it has not
	/// been answered.
	pub fn password_for(&self) -> Option<&str> {
		self.status.password_for.as_deref()
	}

	/// Forgets that a password was asked for, once answered, until SFTP
	/// asks again.
	pub fn password_answered(&mut self) {
		self.status.password_for = None;
	}

	/// The file chosen to send, if one is.
	pub fn sending(&self) -> Option<SharedString> {
		let sending = self.window.global::<crate::Sftp>().get_sending();
		(!sending.is_empty()).then_some(sending)
	}

	pub fn stop_choosing(&self) {
		self.window
			.global::<crate::Sftp>()
			.set_sending(SharedString::new());
	}
}
