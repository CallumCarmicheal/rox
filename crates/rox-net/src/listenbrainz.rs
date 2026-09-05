//! The ListenBrainz submission API: the two calls rox makes and the
//! payload shapes they take. There's no api identity to speak of here,
//! unlike Last.fm: a user token minted at listenbrainz.org/settings is
//! the whole credential, sent as an `Authorization: Token ...` header, so
//! nothing in this file reads a build key or signs anything.
//!
//! This file only speaks wire. What counts as a listen, when one is sent,
//! and what happens to one that failed all live in the service on top of
//! it. Every call blocks, so the app runs them on the background executor.

use std::fmt;

use serde::Serialize;

const API_ROOT: &str = "https://api.listenbrainz.org/1/";

/// What ListenBrainz files these listens under. Both fields are free
/// text over there; they show on the listen as the player it came from
/// and the client that sent it.
const CLIENT: &str = "rox";

/// A failed call: the HTTP status where the service answered, none where
/// the request never got that far. The message is the service's own
/// `error` string when it sent one, which is the part worth showing.
pub struct ApiError {
    pub status: Option<u16>,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl ApiError {
    /// Whether the same call could plausibly work later. No status is the
    /// offline case, always worth another go. Of the statuses, 429 (rate
    /// limited) and the 5xx family are the service having a moment; a 400
    /// payload rejection and a 401 refusal come back identical every time,
    /// so those stop where they are.
    pub fn retryable(&self) -> bool {
        match self.status {
            None => true,
            Some(429) => true,
            Some(code) => (500..600).contains(&code),
        }
    }

    /// Whether ListenBrainz refused the token itself. That's the one
    /// failure worth putting on screen rather than in the log: every call
    /// this token makes fails the same way until it's replaced.
    pub fn token_rejected(&self) -> bool {
        self.status == Some(401)
    }
}

/// One listen as the submission endpoint takes it.
#[derive(Serialize, Clone)]
pub struct Listen {
    /// When the play began, unix seconds. Omitted for `playing_now`,
    /// which the API rejects a timestamp on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listened_at: Option<u64>,
    pub track_metadata: TrackMetadata,
}

#[derive(Serialize, Clone)]
pub struct TrackMetadata {
    pub artist_name: String,
    pub track_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
    pub additional_info: AdditionalInfo,
}

/// The optional half of the metadata. `duration_ms` is the one that earns
/// its place: without it ListenBrainz can't tell a full play of a short
/// track from a skip through a long one, and the stats show it.
#[derive(Serialize, Clone)]
pub struct AdditionalInfo {
    pub media_player: &'static str,
    pub submission_client: &'static str,
    pub submission_client_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl Default for AdditionalInfo {
    fn default() -> Self {
        AdditionalInfo {
            media_player: CLIENT,
            submission_client: CLIENT,
            submission_client_version: env!("CARGO_PKG_VERSION"),
            duration_ms: None,
        }
    }
}

impl Listen {
    /// One listen from the tags rox holds. An empty album sends nothing
    /// rather than an empty string, since a blank release name over there
    /// is worse than no release name.
    pub fn new(
        artist: String,
        title: String,
        album: String,
        duration_secs: Option<f64>,
        listened_at: Option<u64>,
    ) -> Self {
        Listen {
            listened_at,
            track_metadata: TrackMetadata {
                artist_name: artist,
                track_name: title,
                release_name: (!album.is_empty()).then_some(album),
                additional_info: AdditionalInfo {
                    duration_ms: duration_secs
                        .filter(|d| *d > 0.0)
                        .map(|d| (d * 1000.0).round() as u64),
                    ..AdditionalInfo::default()
                },
            },
        }
    }
}

/// Ask the service whether a token works and who it belongs to. Some name
/// for a token it accepts, None for one it calls invalid, an error for
/// everything else. A 401 is the service calling the token invalid in the
/// other dialect it has for it, so that folds in rather than surfacing as
/// a failure the user can't act on differently.
pub fn validate_token(token: &str) -> Result<Option<String>, ApiError> {
    let value = match request(agent_get("validate-token"), token, None) {
        Ok(value) => value,
        Err(e) if e.token_rejected() => return Ok(None),
        Err(e) => return Err(e),
    };
    if value.get("valid").and_then(|v| v.as_bool()) != Some(true) {
        return Ok(None);
    }
    Ok(Some(
        value
            .get("user_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    ))
}

/// Submit listens. `listen_type` is `"single"` for one played track,
/// `"playing_now"` for the track that just started, `"import"` for a
/// batch. Blocking; the caller runs it off the UI thread.
pub fn submit(token: &str, listen_type: &str, payload: &[Listen]) -> Result<(), ApiError> {
    let body = serde_json::json!({ "listen_type": listen_type, "payload": payload });
    request(agent_post("submit-listens"), token, Some(body)).map(|_| ())
}

fn agent_get(path: &str) -> ureq::Request {
    crate::providers::agent().get(&format!("{API_ROOT}{path}"))
}

fn agent_post(path: &str) -> ureq::Request {
    crate::providers::agent().post(&format!("{API_ROOT}{path}"))
}

/// One call: send it with the token header, read the body whether the
/// service liked it or not, and lift its own `error` string out of a
/// failure. A status error still has a JSON body worth reading, the same
/// shape a success has, which is why both paths parse.
fn request(
    request: ureq::Request,
    token: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, ApiError> {
    let request = request.set("Authorization", &format!("Token {token}"));
    // Serialized here and sent as a string: ureq's send_json needs its
    // json feature, which this crate doesn't take, and every other call
    // in here parses its JSON by hand anyway.
    let sent = match body {
        Some(body) => request
            .set("Content-Type", "application/json")
            .send_string(&body.to_string()),
        None => request.call(),
    };
    // A request that never reached the service gets no status: the next
    // try may well go through. Never stringify a ureq error directly; its
    // Display prints the full URL, which is the leak net_reason exists to
    // stop.
    let transport = |message: String| ApiError {
        status: None,
        message,
    };
    let (status, text) = match sent {
        Ok(response) => (
            None,
            response
                .into_string()
                .map_err(|e| transport(e.to_string()))?,
        ),
        Err(ureq::Error::Status(code, response)) => (
            Some(code),
            response
                .into_string()
                .map_err(|e| transport(e.to_string()))?,
        ),
        Err(e) => return Err(transport(crate::providers::net_reason(&e))),
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if let Some(status) = status {
        let message = value
            .get("error")
            .and_then(|e| e.as_str())
            .map(|e| e.to_string())
            .unwrap_or_else(|| format!("service returned {status}"));
        return Err(ApiError {
            status: Some(status),
            message,
        });
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playing_now_listen_carries_no_timestamp() {
        let listen = Listen::new(
            "Boards of Canada".into(),
            "Roygbiv".into(),
            "Music Has the Right to Children".into(),
            Some(151.0),
            None,
        );
        let json = serde_json::to_value(&listen).unwrap();
        assert!(
            json.get("listened_at").is_none(),
            "the API rejects a timestamp on playing_now: {json}"
        );
        assert_eq!(
            json["track_metadata"]["release_name"],
            serde_json::json!("Music Has the Right to Children")
        );
    }

    #[test]
    fn a_played_listen_names_rox_and_its_length() {
        let listen = Listen::new(
            "Boards of Canada".into(),
            "Roygbiv".into(),
            String::new(),
            Some(151.4),
            Some(1_700_000_000),
        );
        let json = serde_json::to_value(&listen).unwrap();
        assert_eq!(json["listened_at"], serde_json::json!(1_700_000_000u64));
        let info = &json["track_metadata"]["additional_info"];
        assert_eq!(info["media_player"], serde_json::json!("rox"));
        assert_eq!(info["submission_client"], serde_json::json!("rox"));
        assert_eq!(info["duration_ms"], serde_json::json!(151_400u64));
        // An empty album is left out rather than sent blank.
        assert!(
            json["track_metadata"].get("release_name").is_none(),
            "{json}"
        );
    }

    #[test]
    fn only_service_side_failures_are_worth_another_try() {
        let api = |status: Option<u16>| ApiError {
            status,
            message: "service said no".to_string(),
        };
        assert!(api(None).retryable(), "the request never landed");
        assert!(api(Some(429)).retryable(), "rate limited");
        assert!(api(Some(503)).retryable(), "service unavailable");
        assert!(!api(Some(400)).retryable(), "a payload it will never take");
        assert!(!api(Some(401)).retryable(), "a token that stays refused");
    }

    #[test]
    fn only_a_401_condemns_the_token() {
        let api = |status: Option<u16>| ApiError {
            status,
            message: "service said no".to_string(),
        };
        assert!(api(Some(401)).token_rejected());
        assert!(
            !api(Some(400)).token_rejected(),
            "the payload, not the token"
        );
        assert!(
            !api(None).token_rejected(),
            "offline says nothing about the token"
        );
    }
}
