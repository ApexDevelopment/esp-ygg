//! A TLS client driven by the bytes a stream carries, rather than by a
//! socket, for Gemini. Gemini servers mostly sign their own certificates, so
//! any certificate is accepted here, and the caller remembers which one each
//! server showed first.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;

use rustls::client::UnbufferedClientConnection;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, InsufficientSizeError};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// A SHA-256 hash of a server's certificate.
pub type Fingerprint = [u8; 32];

/// Settings shared by every connection, built once.
pub fn config() -> Result<Arc<ClientConfig>, String> {
	let provider = Arc::new(rustls_rustcrypto::provider());
	let verifier = Arc::new(AnyCertificate {
		algorithms: provider.signature_verification_algorithms,
	});
	let mut config = ClientConfig::builder_with_details(provider, Arc::new(NoClock))
		.with_safe_default_protocol_versions()
		.map_err(|error| error.to_string())?
		.dangerous()
		.with_custom_certificate_verifier(verifier)
		.with_no_client_auth();
	// Each request is its own connection, and resuming would need a clock.
	config.resumption = rustls::client::Resumption::disabled();
	Ok(Arc::new(config))
}

/// What one round of [`Session::process`] produced.
#[derive(Default)]
pub struct Output {
	/// Encrypted bytes to send to the server.
	pub send: Vec<u8>,
	/// Decrypted bytes from the server.
	pub received: Vec<u8>,
	/// The handshake finished in this round.
	pub connected: bool,
	/// The server said it is done.
	pub closed: bool,
}

pub struct Session {
	connection: UnbufferedClientConnection,
	/// Received bytes that do not yet make a whole record.
	incoming: Vec<u8>,
	/// Data waiting for the handshake to finish.
	pending: Vec<u8>,
	connected: bool,
	/// The server closed, so say goodbye too, once.
	closing: bool,
}

impl Session {
	pub fn new(config: Arc<ClientConfig>, host: &str) -> Result<Self, String> {
		let name = ServerName::try_from(host.to_string()).map_err(|error| error.to_string())?;
		let connection =
			UnbufferedClientConnection::new(config, name).map_err(|error| error.to_string())?;
		Ok(Session {
			connection,
			incoming: Vec::new(),
			pending: Vec::new(),
			connected: false,
			closing: false,
		})
	}

	/// Queues data to encrypt, sent once the handshake allows.
	pub fn write(&mut self, data: &[u8]) {
		self.pending.extend_from_slice(data);
	}

	pub fn receive(&mut self, data: &[u8]) {
		self.incoming.extend_from_slice(data);
	}

	/// The certificate the server showed, once it has.
	pub fn fingerprint(&self) -> Option<Fingerprint> {
		let certificate = self.connection.peer_certificates()?.first()?;
		Some(Sha256::digest(certificate.as_ref()).into())
	}

	/// Moves the connection along as far as what was received allows.
	pub fn process(&mut self) -> Result<Output, String> {
		let mut output = Output::default();
		loop {
			let status = self.connection.process_tls_records(&mut self.incoming);
			let mut stop = false;
			match status.state.map_err(|error| error.to_string())? {
				ConnectionState::ReadTraffic(mut traffic) => {
					while let Some(record) = traffic.next_record() {
						let record = record.map_err(|error| error.to_string())?;
						output.received.extend_from_slice(record.payload);
					}
				}
				ConnectionState::PeerClosed => {
					output.closed = true;
					self.closing = true;
				}
				ConnectionState::Closed => {
					output.closed = true;
					stop = true;
				}
				ConnectionState::EncodeTlsData(mut encoder) => {
					grow_until_fits(&mut output.send, |buffer| {
						encoder.encode(buffer).map_err(|error| match error {
							EncodeError::InsufficientSize(size) => Ok(size),
							other => Err(other.to_string()),
						})
					})?;
				}
				ConnectionState::TransmitTlsData(transmit) => transmit.done(),
				ConnectionState::BlockedHandshake => stop = true,
				ConnectionState::WriteTraffic(mut writer) => {
					if !self.connected {
						self.connected = true;
						output.connected = true;
					}
					if self.closing {
						self.closing = false;
						grow_until_fits(&mut output.send, |buffer| {
							writer
								.queue_close_notify(buffer)
								.map_err(|error| match error {
									EncryptError::InsufficientSize(size) => Ok(size),
									other => Err(other.to_string()),
								})
						})?;
					} else if !self.pending.is_empty() {
						let pending = core::mem::take(&mut self.pending);
						grow_until_fits(&mut output.send, |buffer| {
							writer
								.encrypt(&pending, buffer)
								.map_err(|error| match error {
									EncryptError::InsufficientSize(size) => Ok(size),
									other => Err(other.to_string()),
								})
						})?;
					}
					stop = true;
				}
				_ => stop = true,
			}
			let discard = status.discard;
			self.incoming.drain(..discard);
			if stop {
				return Ok(output);
			}
		}
	}
}

/// Appends what `write` produces to `buffer`, growing it as asked.
fn grow_until_fits(
	buffer: &mut Vec<u8>,
	mut write: impl FnMut(&mut [u8]) -> Result<usize, Result<InsufficientSizeError, String>>,
) -> Result<(), String> {
	let start = buffer.len();
	let mut room = 512;
	loop {
		buffer.resize(start + room, 0);
		match write(&mut buffer[start..]) {
			Ok(written) => {
				buffer.truncate(start + written);
				return Ok(());
			}
			Err(Ok(InsufficientSizeError { required_size })) => room = required_size,
			Err(Err(error)) => {
				buffer.truncate(start);
				return Err(error);
			}
		}
	}
}

/// Accepts any certificate, but still checks that the server holds its key.
#[derive(Debug)]
struct AnyCertificate {
	algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AnyCertificate {
	fn verify_server_cert(
		&self,
		_end_entity: &CertificateDer<'_>,
		_intermediates: &[CertificateDer<'_>],
		_server_name: &ServerName<'_>,
		_ocsp_response: &[u8],
		_now: UnixTime,
	) -> Result<ServerCertVerified, rustls::Error> {
		Ok(ServerCertVerified::assertion())
	}

	fn verify_tls12_signature(
		&self,
		message: &[u8],
		certificate: &CertificateDer<'_>,
		signature: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls12_signature(message, certificate, signature, &self.algorithms)
	}

	fn verify_tls13_signature(
		&self,
		message: &[u8],
		certificate: &CertificateDer<'_>,
		signature: &DigitallySignedStruct,
	) -> Result<HandshakeSignatureValid, rustls::Error> {
		rustls::crypto::verify_tls13_signature(message, certificate, signature, &self.algorithms)
	}

	fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
		self.algorithms.supported_schemes()
	}
}

/// Certificates are not checked for expiry and sessions are not resumed,
/// so nothing needs the real time.
#[derive(Debug)]
struct NoClock;

impl TimeProvider for NoClock {
	fn current_time(&self) -> Option<UnixTime> {
		Some(UnixTime::since_unix_epoch(Duration::ZERO))
	}
}

#[cfg(test)]
pub(crate) mod tests {
	extern crate std;

	use std::io::{Read, Write};

	use rustls::pki_types::PrivateKeyDer;
	use rustls::{ServerConfig, ServerConnection};

	use super::*;

	const CERTIFICATE: &[u8] = include_bytes!("../tests/data/server.crt.der");
	const KEY: &[u8] = include_bytes!("../tests/data/server.key.der");

	pub(crate) fn server(
		versions: &[&'static rustls::SupportedProtocolVersion],
	) -> ServerConnection {
		let provider = Arc::new(rustls_rustcrypto::provider());
		let key = PrivateKeyDer::try_from(KEY.to_vec()).unwrap();
		let config = ServerConfig::builder_with_provider(provider)
			.with_protocol_versions(versions)
			.unwrap()
			.with_no_client_auth()
			.with_single_cert(alloc::vec![CertificateDer::from(CERTIFICATE.to_vec())], key)
			.unwrap();
		ServerConnection::new(Arc::new(config)).unwrap()
	}

	/// Sends a request, answers it as a Gemini server would, and returns
	/// what the client read.
	fn exchange(server: &mut ServerConnection) -> (Vec<u8>, Option<Fingerprint>) {
		let mut client = Session::new(config().unwrap(), "200::1").unwrap();
		let mut received = Vec::new();
		let mut answered = false;
		for _ in 0..20 {
			let mut output = client.process().unwrap();
			received.extend(&output.received);
			if output.connected {
				client.write(b"gemini://[200::1]/\r\n");
				output.send.extend(client.process().unwrap().send);
			}
			if output.closed {
				// The client says goodbye too.
				server.read_tls(&mut &output.send[..]).unwrap();
				let state = server.process_new_packets().unwrap();
				assert!(state.peer_has_closed());
				return (received, client.fingerprint());
			}
			server.read_tls(&mut &output.send[..]).unwrap();
			server.process_new_packets().unwrap();
			let mut request = Vec::new();
			let _ = server.reader().read_to_end(&mut request);
			if !answered && request.ends_with(b"\r\n") {
				server
					.writer()
					.write_all(b"20 text/gemini\r\n# Hello\n")
					.unwrap();
				server.send_close_notify();
				answered = true;
			}
			let mut to_client = Vec::new();
			while server.wants_write() {
				server.write_tls(&mut to_client).unwrap();
			}
			client.receive(&to_client);
		}
		panic!("the exchange never finished");
	}

	#[test]
	fn talks_tls13() {
		let (received, fingerprint) = exchange(&mut server(&[&rustls::version::TLS13]));
		assert_eq!(received, b"20 text/gemini\r\n# Hello\n");
		let expected: Fingerprint = Sha256::digest(CERTIFICATE).into();
		assert_eq!(fingerprint, Some(expected));
	}

	#[test]
	fn talks_tls12() {
		let (received, _) = exchange(&mut server(&[&rustls::version::TLS12]));
		assert_eq!(received, b"20 text/gemini\r\n# Hello\n");
	}
}
