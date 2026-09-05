//! ListenBrainz submission, the second scrobble destination beside
//! Last.fm. This entity owns no clock of its own: it rides the two
//! signals the scrobbler already emits, [`Started`] when a track comes
//! under watch and [`Crossed`] when a play crosses the scrobble
//! threshold. That's deliberate. The threshold is one knob for every
//! destination, so deciding when a play counts a second time here could
//! only ever drift from the first.
//!
//! What it does own is the sending: a start becomes a `playing_now`
//! update, a listen becomes a submission, and listens that failed on a
//! bad network wait in a bounded backlog that rides out with the next one
//! that lands. The wire calls block, so they run on the background
//! executor; failures log and never touch playback.
//!
//! The credential is a user token from listenbrainz.org, kept in
//! `accounts.json` beside the Last.fm session. There's no api identity
//! and no auth dance, so "connected" here means nothing more than a token
//! the service answered a name for.

use gpui::{Context, Entity, Subscription};

use rox_core::settings::Settings;
use rox_net::listenbrainz::{self, Listen};

use crate::lastfm::{Crossed, Scrobbler, Started};

/// How many failed listens are worth holding. A backlog is for a network
/// that dropped for an afternoon, not for an archive: past this the
/// oldest go, because the newest are the ones a user would notice
/// missing.
const BACKLOG_CAP: usize = 100;

/// Where the connection stands, for the settings readout. Unverified is
/// the in-flight check; a token that's never been checked doesn't sit
/// there, it gets checked.
#[derive(Clone, PartialEq)]
pub enum Status {
    /// No token, so nothing is sent and nothing is wrong.
    Off,
    /// A validate call is in flight.
    Unverified,
    /// The service named the account this token belongs to.
    Connected(String),
    /// The service called the token invalid. Every submission fails the
    /// same way until it's replaced, which is why it's on screen.
    Invalid,
    /// The last call didn't get an answer worth acting on: offline, or
    /// the service having a bad afternoon.
    Failed(String),
}

/// The ListenBrainz publisher, one per workspace beside its scrobbler.
/// Holds the live config the settings window edits and persists through,
/// so nothing reads the accounts file per frame.
pub struct ListenBrainz {
    config: rox_core::settings::ListenBrainz,
    /// Listens that failed to send, resent with the next one that lands.
    /// Bounded by [`BACKLOG_CAP`]; the oldest drop first.
    backlog: Vec<Listen>,
    /// Whether a token check is in flight, so a second Connect click
    /// doesn't race the first.
    validating: bool,
    /// Whether a submission is in flight, so two listens close together
    /// don't send the same backlog twice.
    sending: bool,
    status: Status,
    _started: Subscription,
    _crossed: Subscription,
}

impl ListenBrainz {
    pub fn new(scrobbler: &Entity<Scrobbler>, cx: &mut Context<Self>) -> Self {
        // Both signals come off the scrobbler rather than the player: the
        // threshold and the tag resolution are already done there, and
        // doing either again here would be a second answer to a question
        // that has one.
        let _started = cx.subscribe(scrobbler, |this: &mut Self, _, event: &Started, cx| {
            this.now_playing(event, cx);
        });
        let _crossed = cx.subscribe(scrobbler, |this: &mut Self, _, event: &Crossed, cx| {
            this.crossed(event, cx);
        });
        let config = Settings::load().accounts.listenbrainz;
        let status = if config.token.is_empty() {
            Status::Off
        } else {
            match &config.username {
                Some(name) => Status::Connected(name.clone()),
                None => Status::Unverified,
            }
        };
        let mut this = ListenBrainz {
            config,
            backlog: Vec::new(),
            validating: false,
            sending: false,
            status,
            _started,
            _crossed,
        };
        // A token with no name against it was stored by a check that never
        // came back. Ask again rather than leaving the page saying
        // nothing: it's one small GET, and only on this one case.
        if this.status == Status::Unverified {
            this.validate(cx);
        }
        this
    }

    /// The live config, the settings window's read.
    pub fn config(&self) -> &rox_core::settings::ListenBrainz {
        &self.config
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// How many listens are waiting on the network, for the settings
    /// readout.
    pub fn pending(&self) -> usize {
        self.backlog.len()
    }

    /// Whether a token is in hand, which is the whole connection. A token
    /// the service hasn't named an account for still sends; validation is
    /// for the readout, not a gate. The scrobble switch isn't asked here:
    /// it's the scrobbler's, and nothing rides its events while it's off.
    pub fn connected(&self) -> bool {
        !self.config.token.is_empty()
    }

    fn persist(&self) {
        let config = self.config.clone();
        Settings::update(move |s| s.accounts.listenbrainz = config);
    }

    /// Store a token and ask the service who it belongs to. The token is
    /// saved before the check, so a name that never comes back costs a
    /// retry rather than a re-paste.
    pub fn set_token(&mut self, token: String, cx: &mut Context<Self>) {
        self.config.token = token.trim().to_string();
        self.config.username = None;
        self.persist();
        if self.config.token.is_empty() {
            self.status = Status::Off;
            cx.notify();
            return;
        }
        self.validate(cx);
    }

    /// Drop the connection: the token, the name, and anything that hadn't
    /// gone yet. Nothing is revoked over there; a token is only revoked
    /// on the site.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.config.token.clear();
        self.config.username = None;
        self.persist();
        self.backlog.clear();
        self.status = Status::Off;
        cx.notify();
    }

    /// Check the stored token and take the account name off it.
    fn validate(&mut self, cx: &mut Context<Self>) {
        if self.validating || self.config.token.is_empty() {
            return;
        }
        self.validating = true;
        self.status = Status::Unverified;
        cx.notify();
        let token = self.config.token.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { listenbrainz::validate_token(&token) })
                .await;
            this.update(cx, |this, cx| {
                this.validating = false;
                match result {
                    Ok(Some(name)) => {
                        this.config.username = Some(name.clone());
                        this.status = Status::Connected(name);
                        this.persist();
                    }
                    Ok(None) => {
                        this.config.username = None;
                        this.status = Status::Invalid;
                        this.persist();
                    }
                    Err(e) => {
                        log::warn!("listenbrainz: validate: {e}");
                        this.status = Status::Failed(e.message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A track came under watch: tell the service what's on. Never queued
    /// and never retried, since a playing-now that arrives late is worse
    /// than one that never arrives.
    fn now_playing(&mut self, event: &Started, cx: &mut Context<Self>) {
        if !self.connected() || event.artist.is_empty() || event.title.is_empty() {
            return;
        }
        let listen = Listen::new(
            event.artist.clone(),
            event.title.clone(),
            event.album.clone(),
            event.duration_secs,
            None,
        );
        let token = self.config.token.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { listenbrainz::submit(&token, "playing_now", &[listen]) })
                .await;
            if let Err(e) = result {
                let rejected = e.token_rejected();
                log::warn!("listenbrainz: playing now: {e}");
                if rejected {
                    this.update(cx, |this, cx| this.token_rejected(cx)).ok();
                }
            }
        })
        .detach();
    }

    /// A play crossed the threshold: queue it and try to clear everything
    /// waiting.
    fn crossed(&mut self, event: &Crossed, cx: &mut Context<Self>) {
        if !self.connected() || event.artist.is_empty() || event.title.is_empty() {
            return;
        }
        enqueue(
            &mut self.backlog,
            Listen::new(
                event.artist.clone(),
                event.title.clone(),
                event.album.clone(),
                event.duration_secs,
                Some(event.started),
            ),
        );
        self.flush(cx);
    }

    /// Send everything waiting in one call: `single` for the one listen
    /// that just happened, `import` where a dropped network left more
    /// than one behind.
    fn flush(&mut self, cx: &mut Context<Self>) {
        if self.sending || self.backlog.is_empty() || !self.connected() {
            return;
        }
        self.sending = true;
        let batch = self.backlog.clone();
        let count = batch.len();
        let listen_type = if count == 1 { "single" } else { "import" };
        let token = self.config.token.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { listenbrainz::submit(&token, listen_type, &batch) })
                .await;
            this.update(cx, |this, cx| {
                this.sending = false;
                match result {
                    Ok(()) => {
                        // Drain what landed, not the whole list: a track
                        // that crossed the rule while this was in flight
                        // is still owed.
                        landed(&mut this.backlog, count);
                        // A send that landed after a stretch of failures
                        // means the connection is back; the name comes
                        // off a check, not off the submission.
                        if !matches!(this.status, Status::Connected(_)) {
                            this.validate(cx);
                        }
                        if !this.backlog.is_empty() {
                            this.flush(cx);
                        }
                    }
                    Err(e) if e.token_rejected() => {
                        log::warn!("listenbrainz: submit: {e}");
                        this.token_rejected(cx);
                    }
                    Err(e) => {
                        log::warn!("listenbrainz: submit: {e}");
                        // A payload the service will never take stays
                        // rejected however long it waits, so it goes
                        // rather than blocking everything behind it.
                        if !e.retryable() {
                            landed(&mut this.backlog, count);
                        }
                        this.status = Status::Failed(e.message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The service refused the token. The backlog stays: a token pasted
    /// in its place sends it. The name goes, so the page stops claiming
    /// an account that isn't answering.
    fn token_rejected(&mut self, cx: &mut Context<Self>) {
        self.config.username = None;
        self.persist();
        self.status = Status::Invalid;
        cx.notify();
    }
}

/// Put a listen at the back of the queue, dropping from the front once
/// it's over the cap.
fn enqueue(backlog: &mut Vec<Listen>, listen: Listen) {
    backlog.push(listen);
    if backlog.len() > BACKLOG_CAP {
        backlog.drain(..backlog.len() - BACKLOG_CAP);
    }
}

/// Take the front `count` off, what a successful send clears.
fn landed(backlog: &mut Vec<Listen>, count: usize) {
    backlog.drain(..count.min(backlog.len()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listen(title: &str) -> Listen {
        Listen::new(
            "Boards of Canada".into(),
            title.into(),
            "Geogaddi".into(),
            Some(300.0),
            Some(1_700_000_000),
        )
    }

    fn titles(backlog: &[Listen]) -> Vec<&str> {
        backlog
            .iter()
            .map(|l| l.track_metadata.track_name.as_str())
            .collect()
    }

    #[test]
    fn a_failed_send_keeps_its_listens_in_order() {
        let mut backlog = Vec::new();
        enqueue(&mut backlog, listen("Dawn Chorus"));
        // The send failed, so nothing lands and the next listen queues
        // behind it.
        enqueue(&mut backlog, listen("Julie and Candy"));
        assert_eq!(titles(&backlog), vec!["Dawn Chorus", "Julie and Candy"]);
    }

    #[test]
    fn a_success_clears_what_it_sent_and_nothing_else() {
        let mut backlog = Vec::new();
        enqueue(&mut backlog, listen("Dawn Chorus"));
        enqueue(&mut backlog, listen("Julie and Candy"));
        let sent = backlog.len();
        // One more crossed the rule while the send was in flight.
        enqueue(&mut backlog, listen("Alpha and Omega"));
        landed(&mut backlog, sent);
        assert_eq!(
            titles(&backlog),
            vec!["Alpha and Omega"],
            "the one that arrived mid-flight is still owed"
        );
        let sent = backlog.len();
        landed(&mut backlog, sent);
        assert!(backlog.is_empty());
    }

    #[test]
    fn the_cap_drops_the_oldest() {
        let mut backlog = Vec::new();
        for n in 0..BACKLOG_CAP + 5 {
            enqueue(&mut backlog, listen(&format!("track {n}")));
        }
        assert_eq!(backlog.len(), BACKLOG_CAP);
        assert_eq!(
            titles(&backlog).first().copied(),
            Some("track 5"),
            "the five oldest went, the newest stayed"
        );
        let newest = format!("track {}", BACKLOG_CAP + 4);
        assert_eq!(titles(&backlog).last().copied(), Some(newest.as_str()));
    }
}
