use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

use super::gopher::Content;
use super::irc::Phase as IrcPhase;
use super::{App, ConsoleFocus, FIELDS, Field, Group, Lookup, MENU, Screen, TextInput, Uplink};
use crate::time::{DateTime, format_offset};

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
		Screen::Radio => draw_radio(frame, body, app),
		Screen::Gopher => draw_gopher(frame, body, app),
		#[cfg(feature = "gemini")]
		Screen::Gemini => draw_gemini(frame, body, app),
		#[cfg(not(feature = "gemini"))]
		Screen::Gemini => {}
		Screen::Irc => draw_irc(frame, body, app),
		Screen::Ping => draw_ping(frame, body, app),
		Screen::Console => draw_console(frame, body, app),
		Screen::Settings => draw_settings(frame, body, app, FIELDS, app.settings_selected),
		Screen::SettingsGroup(group) => {
			draw_settings(frame, body, app, group.fields(), app.group_selected)
		}
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
		Screen::Radio => "Radio",
		Screen::Gopher => "Gopher",
		Screen::Gemini => "Gemini",
		Screen::Irc => "IRC",
		Screen::Ping => "Ping",
		Screen::Console => "Console",
		Screen::Settings => "Settings",
		Screen::SettingsGroup(Group::Bridge) => "Bridge",
		Screen::SettingsGroup(Group::Hotspot) => "Hotspot",
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
	let mut right = String::from(match uplink {
		Uplink::Off => "",
		Uplink::JoiningWifi => "joining WiFi ",
		Uplink::Connecting => "bridging… ",
		Uplink::Connected => "bridged ",
	});
	match app.status.as_ref().and_then(|status| status.hotspot) {
		Some(0) => right.push_str("hotspot "),
		Some(devices) => right.push_str(&format!("hotspot {devices} ")),
		None => {}
	}
	if let Some(time) = app.status.as_ref().and_then(|status| status.board.time) {
		// While settings are open, preview the time zone being chosen.
		let offset = if matches!(app.screen, Screen::Settings | Screen::SettingsGroup(_)) {
			app.draft.utc_offset
		} else {
			app.saved.utc_offset
		};
		let local = DateTime::from_unix((time / 1_000_000) as i64, offset);
		right.push_str(&local.clock());
		right.push(' ');
	}
	let style = Style::new().fg(Color::Black).bg(ACCENT);
	let [left_area, right_area] = Layout::horizontal([
		Constraint::Fill(1),
		Constraint::Length(right.chars().count() as u16),
	])
	.areas(area);
	frame.render_widget(Paragraph::new(left).style(style), left_area);
	frame.render_widget(Paragraph::new(right.as_str()).style(style), right_area);
}

fn hints(app: &App) -> &'static str {
	match app.screen {
		Screen::Home => "↑↓ select  enter open",
		Screen::Network => "↑↓ select  enter open  esc back",
		Screen::Node(_) => "p ping  m message  esc back",
		Screen::Radio => "esc back",
		Screen::Irc if app.irc.phase == IrcPhase::Setup => "↑↓ field  enter connect  esc back",
		Screen::Irc => "tab buffer  ↑↓ scroll  esc leave",
		Screen::Gopher if app.gopher.search.is_some() => "type a search  enter go",
		Screen::Gopher if app.gopher.editing => "tab complete  enter go",
		Screen::Gopher => match app.gopher.page.as_ref().map(|page| &page.content) {
			Some(Content::Text { .. }) => "↑↓ scroll  g address  esc back",
			_ => "enter open  g address  esc back",
		},
		#[cfg(feature = "gemini")]
		Screen::Gemini => gemini_hints(&app.gemini),
		#[cfg(not(feature = "gemini"))]
		Screen::Gemini => "",
		Screen::Ping if app.pinging => "enter stop  esc back",
		Screen::Ping => "tab complete  enter start",
		Screen::Console => "tab switch field  enter send",
		Screen::Settings | Screen::SettingsGroup(_) if app.editing.is_some() => {
			"enter done  esc cancel"
		}
		Screen::Settings | Screen::SettingsGroup(_) => "↑↓ select  enter change",
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
					status
						.neighbors
						.iter()
						.filter(|neighbor| neighbor.key.is_some())
						.count(),
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
	let [status_area, menu_area] =
		Layout::vertical([Constraint::Length(lines.len() as u16), Constraint::Fill(1)]).areas(area);
	frame.render_widget(Paragraph::new(lines), status_area);
	let mut lines = Vec::new();
	for (index, (label, screen)) in MENU.iter().enumerate() {
		let mut text = format!(" {label}");
		let unread = match screen {
			Screen::Console => app.unread,
			Screen::Irc => app.irc.unread(),
			_ => 0,
		};
		if unread > 0 {
			text.push_str(&format!(" ({unread} new)"));
		}
		let style = if index == app.menu {
			SELECTED
		} else {
			Style::new()
		};
		lines.push(Line::styled(pad(&text, area.width), style));
	}
	// Keep the selection on screen when the menu is longer than the space.
	let scroll = (app.menu + 1).saturating_sub(usize::from(menu_area.height));
	frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), menu_area);
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

fn draw_irc(frame: &mut Frame, area: Rect, app: &App) {
	let client = &app.irc;
	if client.phase == IrcPhase::Setup {
		let mut lines = Vec::new();
		for (index, label) in ["server", "nick", "join"].iter().enumerate() {
			lines.push(field_line(
				label,
				&client.form[index],
				index == client.field,
			));
		}
		if let Some(error) = &client.error {
			lines.push(Line::raw(""));
			lines.push(Line::styled(
				format!(" {error}"),
				Style::new().fg(Color::Yellow),
			));
		}
		frame.render_widget(Paragraph::new(lines), area);
		return;
	}
	let [header, log, input] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Fill(1),
		Constraint::Length(1),
	])
	.areas(area);
	let buffer = &client.buffers[client.current];
	let mut spans = Vec::from([Span::styled(
		format!(" {}", buffer.name),
		Style::new().fg(ACCENT),
	)]);
	if client.phase == IrcPhase::Connecting {
		spans.push(Span::styled(" connecting…", DIM));
	} else {
		spans.push(Span::styled(format!(" as {}", client.nick), DIM));
	}
	for other in client.buffers.iter().filter(|other| other.unread > 0) {
		spans.push(Span::raw(format!(" {}({})", other.name, other.unread)));
	}
	frame.render_widget(Paragraph::new(Line::from(spans)), header);
	let width = usize::from(log.width.saturating_sub(1)).max(1);
	let wrapped: Vec<String> = buffer
		.lines
		.iter()
		.flat_map(|line| wrap(line, width))
		.collect();
	let height = usize::from(log.height);
	let end = wrapped
		.len()
		.saturating_sub(client.scroll.min(wrapped.len().saturating_sub(height)));
	let start = end.saturating_sub(height);
	let lines: Vec<Line> = wrapped[start..end]
		.iter()
		.map(|line| Line::raw(format!(" {line}")))
		.collect();
	frame.render_widget(Paragraph::new(lines), log);
	frame.render_widget(Paragraph::new(field_line(">", &client.input, true)), input);
}

/// Breaks `text` into lines of at most `width` characters, at spaces where
/// it can.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
	let mut lines = Vec::new();
	let mut line = String::new();
	for word in text.split(' ') {
		let mut word = word;
		loop {
			let used = line.chars().count();
			let needed = word.chars().count() + usize::from(used > 0);
			if used + needed <= width {
				if used > 0 {
					line.push(' ');
				}
				line.push_str(word);
				break;
			}
			if used > 0 {
				lines.push(core::mem::take(&mut line));
				continue;
			}
			// A word longer than a line is split.
			let split = word
				.char_indices()
				.nth(width)
				.map_or(word.len(), |(index, _)| index);
			lines.push(word[..split].into());
			word = &word[split..];
			if word.is_empty() {
				break;
			}
		}
	}
	if !line.is_empty() || lines.is_empty() {
		lines.push(line);
	}
	lines
}

fn draw_gopher(frame: &mut Frame, area: Rect, app: &App) {
	let browser = &app.gopher;
	let [top, status, body] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Length(1),
		Constraint::Fill(1),
	])
	.areas(area);
	let top_line = match (&browser.search, &browser.page) {
		(Some((_, _, query)), _) => field_line("search", query, true),
		(None, Some(page)) if !browser.editing => {
			Line::styled(format!(" {}", visible_end(&page.url.to_string(), 38)), DIM)
		}
		_ => field_line("go", &browser.address, browser.editing),
	};
	frame.render_widget(Paragraph::new(top_line), top);
	let status_line = if let Some(error) = &browser.error {
		Line::styled(format!(" {error}"), Style::new().fg(Color::Yellow))
	} else if browser.is_loading() {
		Line::styled(" Loading…", DIM)
	} else {
		Line::raw("")
	};
	frame.render_widget(Paragraph::new(status_line), status);
	let Some(page) = &browser.page else {
		return;
	};
	match &page.content {
		Content::Text { text, scroll } => {
			let text = Paragraph::new(text.as_str())
				.wrap(Wrap { trim: false })
				.scroll((*scroll, 0));
			frame.render_widget(text, body);
		}
		Content::Menu { items, selected } => {
			let lines: Vec<ListItem> = items
				.iter()
				.map(|item| {
					let text = match item.kind {
						'1' => format!("{}/", item.text),
						'7' => format!("{}?", item.text),
						_ => item.text.clone(),
					};
					let style = if item.is_link() && item.is_supported() {
						Style::new()
					} else {
						DIM
					};
					ListItem::new(Line::styled(text, style))
				})
				.collect();
			let list = List::new(lines).highlight_style(SELECTED);
			let mut state = ListState::default().with_selected(Some(*selected));
			frame.render_stateful_widget(list, body, &mut state);
		}
	}
}

#[cfg(feature = "gemini")]
fn gemini_hints(browser: &super::gemini::Browser) -> &'static str {
	if browser.prompt.is_some() {
		"type an answer  enter send"
	} else if browser.editing {
		"tab complete  enter go"
	} else if browser.is_loading() {
		"esc stop"
	} else {
		"↑↓ move  enter open  g address  esc back"
	}
}

#[cfg(feature = "gemini")]
fn draw_gemini(frame: &mut Frame, area: Rect, app: &App) {
	use super::gemini::RowKind;
	let browser = &app.gemini;
	let [top, status, body] = Layout::vertical([
		Constraint::Length(1),
		Constraint::Length(1),
		Constraint::Fill(1),
	])
	.areas(area);
	let top_line = match (&browser.prompt, &browser.page) {
		(Some(prompt), _) => field_line(">", &prompt.answer, true),
		(None, Some(page)) if !browser.editing => {
			Line::styled(format!(" {}", visible_end(&page.url.to_string(), 38)), DIM)
		}
		_ => field_line("go", &browser.address, browser.editing),
	};
	frame.render_widget(Paragraph::new(top_line), top);
	let status_line = if let Some(prompt) = &browser.prompt {
		Line::styled(format!(" {}", prompt.question), Style::new().fg(ACCENT))
	} else if let Some(error) = &browser.error {
		Line::styled(format!(" {error}"), Style::new().fg(Color::Yellow))
	} else if browser.is_loading() {
		Line::styled(" Loading…", DIM)
	} else {
		Line::raw("")
	};
	frame.render_widget(Paragraph::new(status_line), status);
	let Some(page) = &browser.page else {
		return;
	};
	let lines: Vec<Line> = page
		.rows
		.iter()
		.skip(page.scroll)
		.take(usize::from(body.height))
		.map(|row| {
			let style = match row.kind {
				RowKind::Text => Style::new(),
				RowKind::Heading => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
				RowKind::Link(link) if page.selected == Some(link) => SELECTED,
				RowKind::Link(_) => Style::new().fg(Color::Cyan),
				RowKind::Quote | RowKind::Preformatted => DIM,
			};
			Line::styled(row.text.as_str(), style)
		})
		.collect();
	frame.render_widget(Paragraph::new(lines), body);
}

fn draw_radio(frame: &mut Frame, area: Rect, app: &App) {
	let neighbors = app
		.status
		.as_ref()
		.map(|status| status.neighbors.as_slice())
		.unwrap_or_default();
	if neighbors.is_empty() {
		frame.render_widget(
			Paragraph::new(Line::styled(" No ESP-NOW neighbors", DIM)),
			area,
		);
		return;
	}
	let lines: Vec<Line> = neighbors
		.iter()
		.map(|neighbor| {
			let label = match neighbor.key {
				Some(key) => app
					.name_of(&key)
					.map(String::from)
					.unwrap_or_else(|| Ipv6Addr::from(addr_for_key(&key).0).to_string()),
				None => mac_text(&neighbor.mac),
			};
			let label = format!(" {:<17} ", truncate(&label, 17));
			let mut spans = Vec::from([Span::raw(label)]);
			match neighbor.signal {
				Some(signal) => {
					let bars = signal_bars(signal);
					spans.push(Span::styled("▮".repeat(bars), Style::new().fg(ACCENT)));
					spans.push(Span::styled("▮".repeat(4 - bars), DIM));
					spans.push(Span::raw(format!(" {signal:>4} dBm")));
				}
				None => spans.push(Span::styled("    no signal", DIM)),
			}
			if neighbor.key.is_none() {
				spans.push(Span::styled(" linking", DIM));
			} else if let Some(loss) = neighbor.loss {
				spans.push(Span::raw(format!(" {loss:>3}%")));
			}
			Line::from(spans)
		})
		.collect();
	frame.render_widget(Paragraph::new(lines), area);
}

/// Four bars for a strong signal, none for one about to drop out.
fn signal_bars(dbm: i8) -> usize {
	match dbm {
		-55.. => 4,
		-65.. => 3,
		-75.. => 2,
		-85.. => 1,
		_ => 0,
	}
}

fn mac_text(mac: &[u8; 6]) -> String {
	let [a, b, c, d, e, f] = mac;
	format!("{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
}

fn truncate(text: &str, width: usize) -> String {
	if text.chars().count() <= width {
		return text.into();
	}
	let mut short: String = text.chars().take(width - 1).collect();
	short.push('…');
	short
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

fn draw_settings(frame: &mut Frame, area: Rect, app: &App, fields: &[Field], selected: usize) {
	let draft = &app.draft;
	let mut lines = Vec::new();
	for (index, field) in fields.iter().enumerate() {
		let (label, value) = match field {
			Field::Name => ("Name", draft.name.clone()),
			Field::Channel => ("Channel", draft.channel.to_string()),
			Field::TimeZone => (
				"Time zone",
				format!("UTC{}", format_offset(draft.utc_offset)),
			),
			Field::StatusPage => ("Status pages", on_off(draft.status_page)),
			Field::Open(Group::Bridge) => ("Bridge", on_off(draft.bridge)),
			Field::Open(Group::Hotspot) => ("Hotspot", on_off(draft.hotspot)),
			Field::Bridge => ("Bridge", on_off(draft.bridge)),
			Field::Network => ("Network", draft.ssid.clone()),
			Field::Password => ("Password", "•".repeat(draft.password.chars().count())),
			Field::Peer => ("Peer", draft.peer.clone()),
			Field::Hotspot => ("Hotspot", on_off(draft.hotspot)),
			Field::HotspotName => ("Network", draft.hotspot_name.clone()),
			Field::HotspotPassword if draft.hotspot_password.is_empty() => {
				("Password", "none, open".into())
			}
			Field::HotspotPassword => (
				"Password",
				"•".repeat(draft.hotspot_password.chars().count()),
			),
			Field::Save => ("Save and restart", String::new()),
		};
		let selected = index == selected;
		if let (true, Some((editing, input))) = (selected, &app.editing)
			&& editing == field
		{
			let text = visible_end(&input.display(), 25);
			// The old value shows as selected until typing replaces it.
			let value = if input.replaceable {
				Span::styled(text, SELECTED)
			} else {
				Span::styled(
					format!("{text}_"),
					Style::new().add_modifier(Modifier::BOLD),
				)
			};
			lines.push(Line::from(Vec::from([
				Span::raw(format!(" {label:<13}")),
				value,
			])));
			continue;
		}
		let text = format!(" {label:<13}{value}");
		let text = if let Field::Open(_) = field {
			// An arrow at the right edge marks a menu of its own.
			pad(&text, area.width.saturating_sub(2)) + "› "
		} else {
			pad(&text, area.width)
		};
		let style = if selected { SELECTED } else { Style::new() };
		lines.push(Line::styled(text, style));
	}
	let scroll = (selected + 1).saturating_sub(usize::from(area.height));
	frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), area);
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

fn on_off(on: bool) -> String {
	String::from(if on { "on" } else { "off" })
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

#[cfg(test)]
mod tests {
	use super::wrap;

	#[test]
	fn wraps_at_spaces_and_splits_long_words() {
		assert_eq!(wrap("one two three", 7), ["one two", "three"]);
		assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
		assert_eq!(wrap("", 4), [""]);
	}
}
