//! One SSH connection to the board, serving the shared folder over SFTP.
//!
//! A peer that gives the right password is let straight in. Any other is
//! let in only if the person at the board says so: a peer with no password
//! at once, or once a wrong one comes, when there is a password to give.

use core::net::Ipv6Addr;

use embassy_futures::select::{Either3, select3};
use embassy_sync::signal::Signal;
use embedded_io_async::{Read, Write};
use log::{info, warn};
use sunset::{ChanFail, ChanHandle, Error, ServEvent, SignKey};
use sunset_async::{ProgressHolder, SSHServer, SunsetRawMutex};
use sunset_sftp::SftpServerHandler;

use super::files::SharedFiles;
use super::{Attendant, Store};

/// Room for SSH packets each way, enough for SFTP's.
const SSH_BUFFER: usize = 2048;

/// Serves the shared folder to a peer at `from` over a connection already
/// open, until it ends. The host key comes from the node's private key
/// seed, so peers see the node's own key.
pub async fn serve<S: Store, A: Attendant>(
	input: &mut impl Read,
	output: &mut impl Write,
	from: Ipv6Addr,
	seed: &[u8; 32],
	password: Option<&str>,
	store: &mut S,
	attendant: &A,
) -> Result<(), Error> {
	let mut inbuf = [0u8; SSH_BUFFER];
	let mut outbuf = [0u8; SSH_BUFFER];
	let server = SSHServer::new(&mut inbuf, &mut outbuf);
	let host_key = SignKey::Ed25519(sunset::ed25519_dalek::SigningKey::from_bytes(seed));
	let sftp_channel = Signal::<SunsetRawMutex, ChanHandle>::new();

	let events = async {
		let mut session = None;
		loop {
			let mut holder = ProgressHolder::new();
			match server.progress(&mut holder).await? {
				ServEvent::Hostkeys(keys) => keys.hostkeys(&[&host_key])?,
				ServEvent::FirstAuth(mut auth) => match password {
					Some(_) => auth.enable_password_auth(true)?,
					None if attendant.admit(from).await => auth.allow()?,
					None => auth.reject()?,
				},
				ServEvent::PasswordAuth(auth) => {
					let given = auth.password()?;
					if password.is_some_and(|password| same(password, given))
						|| attendant.admit(from).await
					{
						auth.allow()?;
					} else {
						auth.reject()?;
					}
				}
				ServEvent::PubkeyAuth(auth) => auth.reject()?,
				ServEvent::Authenticated => info!("{from} logged in to SFTP"),
				ServEvent::OpenSession(open) if session.is_none() => {
					session = Some(open.accept()?);
				}
				ServEvent::OpenSession(open) => {
					open.reject(ChanFail::SSH_OPEN_ADMINISTRATIVELY_PROHIBITED)?
				}
				ServEvent::SessionSubsystem(request) => {
					match (request.command()?, session.take()) {
						("sftp", Some(channel)) => {
							request.succeed()?;
							sftp_channel.signal(channel);
						}
						_ => request.fail()?,
					}
				}
				ServEvent::SessionShell(request) => request.fail()?,
				ServEvent::SessionExec(request) => request.fail()?,
				ServEvent::SessionPty(request) => request.fail()?,
				ServEvent::SessionEnv(request) => request.fail()?,
				ServEvent::PollAgain => {}
				ServEvent::Defunct => return Ok::<_, Error>(()),
			}
		}
	};

	let sftp = async {
		let channel = sftp_channel.wait().await;
		let (input, output) = server.stdio(channel).await?.split();
		let mut files = SharedFiles::new(store, attendant);
		let mut handler = SftpServerHandler::default();
		let result = handler.run(&mut files, input, output).await;
		files.close_all().await;
		result.map_err(|error| {
			warn!("SFTP with {from} ended: {error:?}");
			Error::ChannelEOF
		})
	};

	match select3(server.run(input, output), events, sftp).await {
		Either3::First(result) | Either3::Second(result) | Either3::Third(result) => result,
	}
}

/// Whether two passwords match, taking as long whatever the difference.
fn same(expected: &str, given: &str) -> bool {
	let (expected, given) = (expected.as_bytes(), given.as_bytes());
	let differences = expected
		.iter()
		.zip(given)
		.fold(0, |differences, (a, b)| differences | (a ^ b));
	expected.len() == given.len() && differences == 0
}
