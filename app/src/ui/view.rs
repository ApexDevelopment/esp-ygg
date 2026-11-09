use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

use super::{App, ConsoleFocus, FIELDS, Field, Lookup, MENU, Screen, TextInput, Uplink};

const ACCENT: Color = Color::Green;
const DIM: Style = Style::new().fg(Color::DarkGray);
const SELECTED: Style = Style::new().fg(Color::Black).bg(ACCENT);

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, app: &App) {
	let [title, body, footer] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Fill(1),
		Constraint::Length(1),
	])
	.areas(frame.area());
	draw_title(frame, title, app);
	match app.screen {
		Screen::Home => draw_home(frame, body, app),
		Screen::Network => draw_network(frame, body, app),
		Screen::Node(key) => draw_node(frame, body, app, key),
		Screen::Ping => draw_ping(frame, body, app),
		Screen::Console => draw_console(frame, body, app),
		Screen::Settings => draw_settings(frame, body, app),
		Screen::Networks => draw_networks(frame, body, app),
	}
	let footer_text = match &app.notice {
		Some(notice) => Line::styled(notice.as_str(), Style::new().fg(Color::Yellow)),
		None => Line::styled(hints(app), DIM),
	};
	frame.render_widget(Paragraph::new(footer_text), footer);
}

fn draw_title(frame: &mut Frame, area: Rect, app: &App) {
	let screen = match app.screen {
		Screen::Home => "",
		Screen::Network | Screen::Node(_) => "Network",
		Screen::Ping => "Ping",
		Screen::Console => "Console",
		Screen::Settings => "Settings",
		Screen::Networks => "WiFi networks",
	};
	let name = if app.saved.name.is_empty() {
		"esp-ygg"
	} else {
		&app.saved.name
	};
	let left = if screen.is_empty() {
		format!(" {name}")
	} else {
		format!(" {name} / {screen}")
	};
	let uplink = app
		.status
		.as_ref()
		.map_or(Uplink::Off, |status| status.uplink);
	let right = match uplink {
		Uplink::Off => "",
		Uplink::JoiningWifi => "joining WiFi ",
		Uplink::Connecting => "bridging… ",
		Uplink::Connected => "bridged ",
	};
	let style = Style::new().fg(Color::Black).bg(ACCENT);
	let [left_area, right_area] = Layout::horizontal([
		Constraint::Fill(1),
		Constraint::Length(right.chars().count() as u16),
	])
	.areas(area);
	frame.render_widget(Paragraph::new(left).style(style), left_area);
	frame.render_widget(Paragraph::new(right).style(style), right_area);
}

fn hints(app: &App) -> &'static str {
	match app.screen {
		Screen::Home => "↑↓ select  enter open",
		Screen::Network => "↑↓ select  enter open  esc back",
		Screen::Node(_) => "p ping  m message  esc back",
		Screen::Ping if app.pinging => "enter stop  esc back",
		Screen::Ping => "type an address  enter start",
		Screen::Console => "tab switch field  enter send",
		Screen::Settings if app.editing.is_some() => "enter done  esc cancel",
		Screen::Settings => "↑↓ select  enter change",
		Screen::Networks => "enter choose  r rescan  esc back",
	}
}

fn draw_home(frame: &mut Frame, area: Rect, app: &App) {
	let mut lines = Vec::new();
	match &app.status {
		Some(status) => {
			lines.push(Line::raw(format!(" {}", status.address)));
			lines.push(Line::styled(
				format!(
					" {} neighbors  {} peers  {} known",
					status.neighbors,
					status.peers.len(),
					status.known.len()
				),
				DIM,
			));
			let mut detail = format!(" up {}", short_uptime(status.board.uptime.as_secs()));
			if let Some(free) = status.board.free_heap {
				detail.push_str(&format!("  {} KB free", free / 1024));
			}
			lines.push(Line::styled(detail, DIM));
		}
		None => lines.push(Line::styled(" Starting…", DIM)),
	}
	lines.push(Line::raw(""));
	for (index, (label, screen)) in MENU.iter().enumerate() {
		let mut text = format!(" {label}");
		if *screen == Screen::Console && app.unread > 0 {
			text.push_str(&format!(" ({} new)", app.unread));
		}
		let style = if index == app.menu {
			SELECTED
		} else {
			Style::new()
		};
		lines.push(Line::styled(pad(&text, area.width), style));
	}
	frame.render_widget(Paragraph::new(lines), area);
}

fn short_uptime(seconds: u64) -> String {
	match seconds {
		0..60 => format!("{seconds} s"),
		60..3600 => format!("{} min", seconds / 60),
		3600..86_400 => format!("{} h {} min", seconds / 3600, seconds / 60 % 60),
		_ => format!("{} d {} h", seconds / 86_400, seconds / 3600 % 24),
	}
}

fn draw_network(frame: &mut Frame, area: Rect, app: &App) {
	let nodes = app.nodes();
	if nodes.is_empty() {
		let text = Line::styled(" No other nodes yet", DIM);
		frame.render_widget(Paragraph::new(text), area);
		return;
	}
	let peers = app
		.status
		.as_ref()
		.map(|status| status.peers.clone())
		.unwrap_or_default();
	let items: Vec<ListItem> = nodes
		.iter()
		.map(|key| {
			let marker = if peers.contains(key) { "•" } else { " " };
			let label = match app.name_of(key) {
				Some(name) => name.to_string(),
				None => Ipv6Addr::from(addr_for_key(key).0).to_string(),
			};
			let style = match app.lookups.get(key) {
				Some(Lookup::Done { .. }) => Style::new(),
				_ => DIM,
			};
			ListItem::new(Line::styled(format!("{marker}{label}"), style))
		})
		.collect();
	let list = List::new(items).highlight_style(SELECTED);
	let mut state = ListState::default().with_selected(Some(app.network_selected));
	frame.render_stateful_widget(list, area, &mut state);
}

fn draw_node(frame: &mut Frame, area: Rect, app: &App, key: yggdrasil_espnow::PublicKey) {
	let address = Ipv6Addr::from(addr_for_key(&key).0);
	let mut lines = Vec::from([
		Line::raw(format!(" {address}")),
		Line::styled(format!(" key {}", Hex(&key.0[..8])), DIM),
	]);
	match app.lookups.get(&key) {
		Some(Lookup::Done { fields, .. }) => {
			for (name, value) in fields {
				lines.push(Line::from(Vec::from([
					Span::styled(format!(" {name} "), DIM),
					Span::raw(value.clone()),
				])));
			}
		}
		Some(Lookup::Failed) => lines.push(Line::styled(" No nodeinfo answer", DIM)),
		_ => lines.push(Line::styled(" Asking for nodeinfo…", DIM)),
	}
	frame.render_widget(Paragraph::new(lines), area);
}

fn draw_ping(frame: &mut Frame, area: Rect, app: &App) {
	let [target, summary, log] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Length(1),
		Constraint::Fill(1),
	])
	.areas(area);
	frame.render_widget(
		Paragraph::new(field_line("to", &app.ping_target, !app.pinging)),
		target,
	);
	let mut text = format!(" sent {}  received {}", app.ping_sent, app.ping_received);
	if app.ping_received > 0 {
		let average = app.ping_total / app.ping_received;
		text.push_str(&format!("  avg {} ms", average.as_millis()));
	}
	frame.render_widget(Paragraph::new(Line::styled(text, DIM)), summary);
	frame.render_widget(Paragraph::new(tail(&app.ping_log, log.height)), log);
}

fn draw_console(frame: &mut Frame, area: Rect, app: &App) {
	let [log, target, message] = Layout::vertical([
		Constraint::Fill(1),
		Constraint::Length(1),
		Constraint::Length(1),
	])
	.areas(area);
	let lines = if app.console_log.is_empty() {
		Vec::from([Line::styled(
			format!(" Listening on UDP port {}", crate::CONSOLE_PORT),
			DIM,
		)])
	} else {
		tail(&app.console_log, log.height)
	};
	frame.render_widget(Paragraph::new(lines), log);
	let focus = app.console_focus;
	frame.render_widget(
		Paragraph::new(field_line(
			"to",
			&app.console_target,
			focus == ConsoleFocus::Target,
		)),
		target,
	);
	frame.render_widget(
		Paragraph::new(field_line(
			">",
			&app.console_message,
			focus == ConsoleFocus::Message,
		)),
		message,
	);
}

fn draw_settings(frame: &mut Frame, area: Rect, app: &App) {
	let draft = &app.draft;
	let lines: Vec<Line> = FIELDS
		.iter()
		.enumerate()
		.map(|(index, field)| {
			let (label, value) = match field {
				Field::Name => ("Name", draft.name.clone()),
				Field::Bridge => (
					"Bridge",
					String::from(if draft.bridge { "on" } else { "off" }),
				),
				Field::Network => ("WiFi", draft.ssid.clone()),
				Field::Password => ("Password", "•".repeat(draft.password.chars().count())),
				Field::Peer => ("Peer", draft.peer.clone()),
				Field::Channel => ("Channel", draft.channel.to_string()),
				Field::Save => ("Save and restart", String::new()),
			};
			let selected = index == app.settings_selected;
			if let (true, Some((editing, input))) = (selected, &app.editing)
				&& editing == field
			{
				return Line::from(Vec::from([
					Span::raw(format!(" {label:<9}")),
					Span::styled(format!("{}_", visible_end(&input.display(), 29)), SELECTED),
				]));
			}
			let text = format!(" {label:<9}{value}");
			let style = if selected { SELECTED } else { Style::new() };
			Line::styled(pad(&text, area.width), style)
		})
		.collect();
	frame.render_widget(Paragraph::new(lines), area);
}

fn draw_networks(frame: &mut Frame, area: Rect, app: &App) {
	let Some(networks) = &app.networks else {
		frame.render_widget(Paragraph::new(Line::styled(" Scanning…", DIM)), area);
		return;
	};
	if networks.is_empty() {
		frame.render_widget(
			Paragraph::new(Line::styled(" No networks found", DIM)),
			area,
		);
		return;
	}
	let items: Vec<ListItem> = networks
		.iter()
		.map(|network| {
			let lock = if network.secured { "" } else { " open" };
			ListItem::new(format!(" {}  {} dBm{lock}", network.ssid, network.signal))
		})
		.collect();
	let list = List::new(items).highlight_style(SELECTED);
	let mut state = ListState::default().with_selected(Some(app.networks_selected));
	frame.render_stateful_widget(list, area, &mut state);
}

/// A labelled text field, highlighted with a cursor when focused.
fn field_line<'a>(label: &'a str, input: &TextInput, focused: bool) -> Line<'a> {
	let text = visible_end(&input.display(), 34);
	let value = if focused {
		Span::styled(
			format!("{text}_"),
			Style::new().add_modifier(Modifier::BOLD),
		)
	} else {
		Span::raw(text)
	};
	Line::from(Vec::from([Span::styled(format!(" {label} "), DIM), value]))
}

/// The last `width` characters of `text`, so the end of a long entry stays
/// in view while typing.
fn visible_end(text: &str, width: usize) -> String {
	let count = text.chars().count();
	text.chars().skip(count.saturating_sub(width)).collect()
}

fn tail(log: &alloc::collections::VecDeque<String>, height: u16) -> Vec<Line<'static>> {
	let skip = log.len().saturating_sub(usize::from(height));
	log.iter()
		.skip(skip)
		.map(|line| Line::raw(format!(" {line}")))
		.collect()
}

/// Pads `text` so a highlighted line spans the full width.
fn pad(text: &str, width: u16) -> String {
	format!("{text:<width$}", width = usize::from(width))
}

struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))?;
		f.write_str("…")
	}
}
