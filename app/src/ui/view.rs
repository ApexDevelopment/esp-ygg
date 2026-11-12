use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::Ipv6Addr;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use yggdrasil_espnow::yggdrasil_proto::address::addr_for_key;

#[cfg(feature = "apps")]
use super::APPS;
use super::directory::Lookup;
use super::{App, FIELDS, Field, Group, MENU, MENUS, Screen, TextInput, Uplink};
use crate::time::{format_offset, short_uptime};

/// Where the speaker icon's two cells start, counted from the title
/// bar's right edge.
#[cfg(feature = "voice")]
pub const SPEAKER_FROM_RIGHT: u16 = 3;

pub(super) const ACCENT: Color = Color::Green;
pub(super) const DIM: Style = Style::new().fg(Color::DarkGray);
pub(super) const SELECTED: Style = Style::new().fg(Color::Black).bg(ACCENT);

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, app: &App) {
	app.drawn.set(frame.area());
	let mut screen = frame.area();
	if let Some((keyboard, kind)) = app.keyboard_area() {
		screen = split_bottom(screen, keyboard.height).0;
		app.keyboard.draw(frame, keyboard, kind);
	}
	let (title, rest) = split_top(screen, 1);
	let (body, footer) = split_bottom(rest, 1);
	draw_title(frame, title, app);
	match app.screen {
		Screen::Home => draw_home(frame, body, app),
		#[cfg(feature = "apps")]
		Screen::Apps => draw_menu(frame, body, app, APPS, app.apps_selected),
		Screen::Nodes => draw_nodes(frame, body, app),
		Screen::Node(key) => draw_node(frame, body, app, key),
		Screen::Neighbors => draw_neighbors(frame, body, app),
		#[cfg(feature = "gopher")]
		Screen::Gopher => app.gopher.draw(frame, body),
		#[cfg(feature = "gemini")]
		Screen::Gemini => app.gemini.draw(frame, body),
		#[cfg(feature = "voice")]
		Screen::Call => {
			let who = app.call.peer.as_ref().map(|(address, name)| {
				name.clone()
					.unwrap_or_else(|| app.short_name_of_address(address))
			});
			app.call
				.draw(frame, body, app.now, who.as_deref().unwrap_or_default())
		}
		#[cfg(feature = "irc")]
		Screen::Irc => app.irc.draw(frame, body),
		Screen::Ping => app.ping.draw(frame, body),
		#[cfg(feature = "console")]
		Screen::Console => app.console.draw(frame, body),
		Screen::Settings => draw_settings(frame, body, app, FIELDS, app.settings_selected),
		Screen::SettingsGroup(group) => {
			draw_settings(frame, body, app, group.fields(), app.group_selected)
		}
		Screen::WifiNetworks => draw_wifi_networks(frame, body, app),
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
		#[cfg(feature = "apps")]
		Screen::Apps => "Apps",
		Screen::Nodes | Screen::Node(_) => "Nodes",
		Screen::Neighbors => "Neighbors",
		#[cfg(feature = "gopher")]
		Screen::Gopher => "Gopher",
		#[cfg(feature = "gemini")]
		Screen::Gemini => "Gemini",
		#[cfg(feature = "voice")]
		Screen::Call => "Call",
		#[cfg(feature = "irc")]
		Screen::Irc => "IRC",
		Screen::Ping => "Ping",
		#[cfg(feature = "console")]
		Screen::Console => "Console",
		Screen::Settings => "Settings",
		Screen::SettingsGroup(Group::Bridge) => "Bridge",
		Screen::SettingsGroup(Group::Hotspot) => "Hotspot",
		#[cfg(feature = "voice")]
		Screen::SettingsGroup(Group::Ringer) => "Ringer",
		Screen::WifiNetworks => "WiFi networks",
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
	if let Some(local) = app.local_time() {
		right.push_str(&local.clock());
		right.push(' ');
	}
	// Blank cells for the host to draw the speaker icon in.
	#[cfg(feature = "voice")]
	right.push_str("   ");
	let style = Style::new().fg(Color::Black).bg(ACCENT);
	let (left_area, right_area) = split_right(area, right.chars().count() as u16);
	frame.render_widget(Paragraph::new(left).style(style), left_area);
	frame.render_widget(Paragraph::new(right.as_str()).style(style), right_area);
}

fn hints(app: &App) -> &'static str {
	match app.screen {
		Screen::Home => "↑↓ select  enter open",
		#[cfg(feature = "apps")]
		Screen::Apps => "↑↓ select  enter open  esc back",
		Screen::Nodes => "↑↓ select  enter open  esc back",
		Screen::Node(_) => "p ping  m message  esc back",
		Screen::Neighbors => "esc back",
		#[cfg(feature = "irc")]
		Screen::Irc => app.irc.hints(),
		#[cfg(feature = "gopher")]
		Screen::Gopher => app.gopher.hints(),
		#[cfg(feature = "gemini")]
		Screen::Gemini => app.gemini.hints(),
		#[cfg(feature = "voice")]
		Screen::Call => app.call.hints(),
		Screen::Ping => app.ping.hints(),
		#[cfg(feature = "console")]
		Screen::Console => app.console.hints(),
		Screen::Settings | Screen::SettingsGroup(_) if app.editing.is_some() => {
			"enter done  esc cancel"
		}
		Screen::Settings | Screen::SettingsGroup(_) => "↑↓ select  enter change",
		Screen::WifiNetworks => "enter choose  r rescan  esc back",
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
					status.connected_neighbors(),
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
	let (status_area, menu_area) = split_top(area, lines.len() as u16);
	frame.render_widget(Paragraph::new(lines), status_area);
	draw_menu(frame, menu_area, app, MENU, app.menu);
}

fn draw_menu(frame: &mut Frame, area: Rect, app: &App, items: &[(&str, Screen)], selected: usize) {
	let mut lines = Vec::new();
	for (index, &(label, screen)) in items.iter().enumerate() {
		let mut text = format!(" {label}");
		let count = unread(app, screen);
		if count > 0 {
			text.push_str(&format!(" ({count} new)"));
		}
		let text = if MENUS.contains(&screen) {
			pad(&text, area.width.saturating_sub(2)) + "› "
		} else {
			pad(&text, area.width)
		};
		let style = if index == selected {
			SELECTED
		} else {
			Style::new()
		};
		lines.push(Line::styled(text, style));
	}
	// Keep the selection on screen when the menu is longer than the space.
	let scroll = (selected + 1).saturating_sub(usize::from(area.height));
	frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), area);
}

/// New messages waiting on a menu item's screen, or in the apps it opens.
fn unread(app: &App, screen: Screen) -> usize {
	#[cfg(feature = "apps")]
	if screen == Screen::Apps {
		return APPS
			.iter()
			.map(|&(_, app_screen)| app_unread(app, app_screen))
			.sum();
	}
	app_unread(app, screen)
}

#[cfg_attr(
	not(any(feature = "console", feature = "irc")),
	allow(unused_variables)
)]
fn app_unread(app: &App, screen: Screen) -> usize {
	match screen {
		#[cfg(feature = "console")]
		Screen::Console => app.console.unread,
		#[cfg(feature = "irc")]
		Screen::Irc => app.irc.unread(),
		_ => 0,
	}
}

fn draw_nodes(frame: &mut Frame, area: Rect, app: &App) {
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
			let style = match app.directory.lookup(key) {
				Some(Lookup::Done { .. }) => Style::new(),
				_ => DIM,
			};
			ListItem::new(Line::styled(format!("{marker}{label}"), style))
		})
		.collect();
	let list = List::new(items).highlight_style(SELECTED);
	let mut state = ListState::default().with_selected(Some(app.nodes_selected));
	frame.render_stateful_widget(list, area, &mut state);
}

fn draw_node(frame: &mut Frame, area: Rect, app: &App, key: yggdrasil_espnow::PublicKey) {
	let address = Ipv6Addr::from(addr_for_key(&key).0);
	let mut lines = Vec::from([
		Line::raw(format!(" {address}")),
		Line::styled(format!(" key {}", Hex(&key.0[..8])), DIM),
	]);
	match app.directory.lookup(&key) {
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

/// Breaks `text` into lines of at most `width` characters, at spaces where
/// it can.
#[cfg(any(feature = "irc", feature = "gemini"))]
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

fn draw_neighbors(frame: &mut Frame, area: Rect, app: &App) {
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

#[cfg(feature = "voice")]
fn ringtone_name(tone: u8) -> &'static str {
	let names = crate::voice::ringtone::RINGTONES;
	names.get(usize::from(tone)).unwrap_or(&names[0])
}

#[cfg(feature = "voice")]
fn volume(level: u8) -> String {
	match level {
		0 => "off".into(),
		_ => format!(
			"{}%",
			u32::from(level) * 100 / u32::from(crate::voice::ringtone::LOUDEST)
		),
	}
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
			Field::Ssid => ("Network", draft.ssid.clone()),
			Field::WifiPassword => ("Password", "•".repeat(draft.password.chars().count())),
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
			#[cfg(feature = "voice")]
			Field::Open(Group::Ringer) if draft.ring_volume == 0 => ("Ringer", "off".into()),
			#[cfg(feature = "voice")]
			Field::Open(Group::Ringer) => (
				"Ringer",
				format!(
					"{}, {}",
					ringtone_name(draft.ringtone),
					volume(draft.ring_volume)
				),
			),
			#[cfg(feature = "voice")]
			Field::Ringtone => ("Ringtone", ringtone_name(draft.ringtone).into()),
			#[cfg(feature = "voice")]
			Field::RingVolume => ("Volume", volume(draft.ring_volume)),
			Field::Save => ("Save and restart", String::new()),
			Field::Export => ("Export to SD card", String::new()),
			Field::Import => ("Import from SD card", String::new()),
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

fn draw_wifi_networks(frame: &mut Frame, area: Rect, app: &App) {
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
pub(super) fn field_line<'a>(label: &'a str, input: &TextInput, focused: bool) -> Line<'a> {
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
pub(super) fn visible_end(text: &str, width: usize) -> String {
	let count = text.chars().count();
	text.chars().skip(count.saturating_sub(width)).collect()
}

pub(super) fn tail(log: &alloc::collections::VecDeque<String>, height: u16) -> Vec<Line<'static>> {
	let skip = log.len().saturating_sub(usize::from(height));
	log.iter()
		.skip(skip)
		.map(|line| Line::raw(format!(" {line}")))
		.collect()
}

/// Splits `area` into its first `rows` rows and the rest. This and the two
/// below stand in for Ratatui's `Layout`, which solves its constraints in
/// floating point and takes milliseconds on chips without an FPU.
pub(super) fn split_top(area: Rect, rows: u16) -> (Rect, Rect) {
	let rows = rows.min(area.height);
	let top = Rect {
		height: rows,
		..area
	};
	let rest = Rect {
		y: area.y + rows,
		height: area.height - rows,
		..area
	};
	(top, rest)
}

/// Splits `area` into all but its last `rows` rows, and those rows.
pub(super) fn split_bottom(area: Rect, rows: u16) -> (Rect, Rect) {
	let (rest, bottom) = split_top(area, area.height.saturating_sub(rows));
	(rest, bottom)
}

/// Splits `area` into all but its last `columns` columns, and those columns.
pub(super) fn split_right(area: Rect, columns: u16) -> (Rect, Rect) {
	let left_width = area.width.saturating_sub(columns);
	let left = Rect {
		width: left_width,
		..area
	};
	let right = Rect {
		x: area.x + left_width,
		width: area.width - left_width,
		..area
	};
	(left, right)
}

/// Pads `text` so a highlighted line spans the full width.
pub(super) fn pad(text: &str, width: u16) -> String {
	format!("{text:<width$}", width = usize::from(width))
}

struct Hex<'a>(&'a [u8]);

impl core::fmt::Display for Hex<'_> {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))?;
		f.write_str("…")
	}
}

#[cfg(all(test, any(feature = "irc", feature = "gemini")))]
mod tests {
	use super::wrap;

	#[test]
	fn wraps_at_spaces_and_splits_long_words() {
		assert_eq!(wrap("one two three", 7), ["one two", "three"]);
		assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
		assert_eq!(wrap("", 4), [""]);
	}
}
