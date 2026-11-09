use alloc::string::String;

/// A one-line text field that is typed into at the end.
#[derive(Clone, Debug, Default)]
pub struct TextInput {
	pub text: String,
	/// Shown as dots, for passwords.
	pub secret: bool,
	max_len: usize,
	/// Whether the text is the field's old value, which the first typed
	/// character replaces.
	pub replaceable: bool,
}

impl TextInput {
	pub fn new(text: &str, max_len: usize) -> Self {
		TextInput {
			text: text.into(),
			secret: false,
			max_len,
			replaceable: false,
		}
	}

	/// A field holding its current value, which typing replaces and
	/// Backspace edits.
	pub fn prefilled(text: &str, max_len: usize) -> Self {
		TextInput {
			replaceable: true,
			..TextInput::new(text, max_len)
		}
	}

	pub fn secret(text: &str, max_len: usize) -> Self {
		TextInput {
			secret: true,
			..TextInput::prefilled(text, max_len)
		}
	}

	pub fn push(&mut self, character: char) {
		if self.replaceable {
			self.text.clear();
			self.replaceable = false;
		}
		if self.text.chars().count() < self.max_len && !character.is_control() {
			self.text.push(character);
		}
	}

	pub fn backspace(&mut self) {
		self.replaceable = false;
		self.text.pop();
	}

	pub fn clear(&mut self) {
		self.text.clear();
	}

	/// What to draw: the text, or dots for a secret.
	pub fn display(&self) -> String {
		if self.secret {
			self.text.chars().map(|_| '•').collect()
		} else {
			self.text.clone()
		}
	}
}
