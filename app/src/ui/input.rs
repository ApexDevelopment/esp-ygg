use alloc::string::String;

/// A one-line text field that is typed into at the end.
#[derive(Clone, Debug, Default)]
pub struct TextInput {
	pub text: String,
	/// Shown as dots, for passwords.
	pub secret: bool,
	max_len: usize,
}

impl TextInput {
	pub fn new(text: &str, max_len: usize) -> Self {
		TextInput {
			text: text.into(),
			secret: false,
			max_len,
		}
	}

	pub fn secret(text: &str, max_len: usize) -> Self {
		TextInput {
			secret: true,
			..TextInput::new(text, max_len)
		}
	}

	pub fn push(&mut self, character: char) {
		if self.text.chars().count() < self.max_len && !character.is_control() {
			self.text.push(character);
		}
	}

	pub fn backspace(&mut self) {
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
