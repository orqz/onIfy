//! onify: what Spotify Connect knows about the account's devices and what
//! plays on them, taken from the cluster updates the dealer sends, for a
//! device picker and for showing playback on another device.

use crate::protocol::connect::Cluster;

/// One of the account's Spotify Connect devices.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConnectDevice {
    /// Its device id.
    pub id: String,
    /// The name people gave it.
    pub name: String,
    /// "computer", "smartphone", "speaker", "tv", … (lowercase device type).
    pub kind: String,
    /// 0 to 65535.
    pub volume: u32,
}

/// The account's devices and the playback on the active one.
#[derive(Debug, Clone, Default)]
pub struct ClusterInfo {
    /// The device playing (or last told to play); empty for none.
    pub active_device_id: String,
    /// Every device Spotify Connect can see, sorted by name.
    pub devices: Vec<ConnectDevice>,
    /// The playing track's uri.
    pub track_uri: String,
    /// What it plays from.
    pub context_uri: String,
    /// Where it was at the time of this update, in ms.
    pub position_ms: i64,
    /// The track's length in ms.
    pub duration_ms: i64,
    /// Playing (not paused or stopped).
    pub playing: bool,
    /// Shuffle is on.
    pub shuffle: bool,
    /// Repeat the context.
    pub repeat_context: bool,
    /// Repeat the track.
    pub repeat_track: bool,
}

impl From<&Cluster> for ClusterInfo {
    fn from(cluster: &Cluster) -> Self {
        let state = &cluster.player_state;
        let playing = state.is_playing && !state.is_paused;
        // The position is as of the state's own timestamp; bring it up to
        // the time this update was sent.
        let mut position_ms = state.position_as_of_timestamp;
        if playing && cluster.server_timestamp_ms > state.timestamp && state.timestamp > 0 {
            position_ms += cluster.server_timestamp_ms - state.timestamp;
        }
        let mut devices: Vec<ConnectDevice> = cluster
            .device
            .iter()
            .filter(|(_, d)| !d.name.is_empty())
            .map(|(id, d)| ConnectDevice {
                id: id.clone(),
                name: d.name.clone(),
                kind: format!("{:?}", d.device_type.enum_value_or_default()).to_lowercase(),
                volume: d.volume,
            })
            .collect();
        devices.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Self {
            active_device_id: cluster.active_device_id.clone(),
            devices,
            track_uri: state.track.uri.clone(),
            context_uri: state.context_uri.clone(),
            position_ms: position_ms.clamp(0, state.duration.max(0)),
            duration_ms: state.duration,
            playing,
            shuffle: state.options.shuffling_context,
            repeat_context: state.options.repeating_context,
            repeat_track: state.options.repeating_track,
        }
    }
}
