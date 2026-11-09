mod common;

use std::time::Duration;

use common::Radio;

#[test]
fn neighbors_peer_over_espnow() {
	let mut radio = Radio::line(2);
	assert!(radio.run_until(Duration::from_secs(10), |radio| radio.all_peered()));
	assert!(radio.ping(0, 1, Duration::from_secs(10)));
	assert!(radio.ping(1, 0, Duration::from_secs(10)));
}

#[test]
fn pings_cross_a_line_of_nodes() {
	let mut radio = Radio::line(5);
	assert!(radio.run_until(Duration::from_secs(20), |radio| radio.all_peered()));
	assert!(radio.ping(0, 4, Duration::from_secs(30)));
	assert!(radio.ping(4, 0, Duration::from_secs(30)));
}

#[test]
fn survives_a_lossy_radio() {
	let mut radio = Radio::line(3);
	radio.loss_every = Some(4);
	assert!(radio.run_until(Duration::from_secs(30), |radio| radio.all_peered()));
	assert!(radio.ping(0, 2, Duration::from_secs(30)));
}

#[test]
fn reroutes_when_a_neighbor_moves_away() {
	// 0 - 1 - 2, and 0 - 3 - 2 as a second route
	let mut radio = Radio::line(3);
	radio.add_node(yggdrasil_espnow::Config::new(common::private_key(9)));
	radio.connect(0, 3);
	radio.connect(3, 2);
	assert!(radio.run_until(Duration::from_secs(20), |radio| radio.all_peered()));
	assert!(radio.ping(0, 2, Duration::from_secs(30)));
	radio.disconnect(1, 2);
	radio.disconnect(0, 1);
	assert!(radio.ping(0, 2, Duration::from_secs(60)));
}
