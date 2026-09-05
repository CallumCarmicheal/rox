//! Libre.fm: Last.fm's protocol at a different host, with no api identity
//! to register. The service takes any key pair at all (its source checks
//! the two for length and nothing else, and the live site doesn't even do
//! that), so rox signs with a fixed pair of its own rather than asking
//! anyone for one, and there's nothing to file sessions under the way
//! ADR 26 has to for Last.fm. The signing, the call, and the error codes
//! are all [`crate::lastfm`]'s; this file only says where to send the
//! form and what to sign it with. The scrobbler built on top is in
//! rox-services.

use std::collections::BTreeMap;

use crate::lastfm::{self, ApiError};

const API_ROOT: &str = "https://libre.fm/2.0/";

/// The page the connect flow opens for the user to authorize a token,
/// with `api_key` and `token` as its query.
pub const AUTH_URL: &str = "https://libre.fm/api/auth/";

/// The pair every rox build signs with. Arbitrary by design: the service
/// registers nothing, so these identify the app in its logs and nothing
/// more. A secret in the source is fine here for the same reason.
pub const API_KEY: &str = "5am5ijte60yp01u33a0xnvxx3tsjb4m3";
const API_SECRET: &str = "92vhtr86swzvnhonisiikwgbb403nutl";

/// One signed Libre.fm call, blocking. Same shape as the Last.fm one
/// minus the secret, which is the build's here rather than the user's.
pub fn call(method: &str, params: BTreeMap<String, String>) -> Result<serde_json::Value, ApiError> {
    lastfm::call_at(API_ROOT, method, API_SECRET, params)
}
