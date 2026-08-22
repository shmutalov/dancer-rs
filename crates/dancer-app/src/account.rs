//! Yandex sign-in and token validation, driven from the GUI (ROADMAP M5).
//!
//! # Why the token is checked on every start
//!
//! OAuth tokens expire, and they are also revoked from the account page — which is
//! exactly what a careful user does after reading that theirs sits in a plain-text
//! config. Finding out lazily means the failure surfaces mid-track as a fetch that
//! quietly does not happen, behind a dancer that carries on looking fine. There is
//! no error anywhere the user will see, and the symptom is "it stopped working
//! sometimes". Checking up front turns that into a sentence and a button.
//!
//! # Everything here runs on a worker thread
//!
//! The check is a network round trip and the sign-in takes as long as the user
//! takes — minutes, if they have to find their phone. Neither may block the event
//! loop, so both run detached and report back through a channel. The dancer keeps
//! dancing throughout, which also means the sign-in dialog is not the only thing on
//! screen while it waits.

use std::sync::mpsc::{Receiver, Sender};

use crate::dialog;

/// Where to revoke a token afterwards. Shown wherever sign-in is confirmed, because
/// the honest pitch for storing a credential in a text file is that it is trivially
/// revocable.
pub const REVOKE_URL: &str = "https://id.yandex.ru/security";

/// What the worker learned.
#[derive(Debug, Clone)]
pub enum AccountEvent {
    /// The stored token works. Carries the account name, for the tray.
    Valid { login: String },
    /// The token was rejected. Distinct from a network failure: only this one means
    /// signing in again would help.
    Rejected,
    /// Could not tell — no network, API down. Deliberately not treated as rejection.
    Unknown(String),
    /// A sign-in completed and produced a token to store.
    SignedIn { token: String, login: String },
    /// A sign-in did not complete. Declined, expired, or cancelled.
    SignInFailed(String),
}

pub struct Channel {
    pub tx: Sender<AccountEvent>,
    pub rx: Receiver<AccountEvent>,
}

impl Channel {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { tx, rx }
    }
}

/// Check a stored token in the background.
///
/// Does nothing when there is no token: an empty token is not a failure, it is a
/// user who has not asked for the feature, and prompting them would be nagging.
pub fn verify(token: String, tx: Sender<AccountEvent>) {
    if token.trim().is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let _ = tx.send(verify_blocking(&token));
    });
}

/// Start the OAuth device flow, with a dialog rather than a console.
///
/// `--yandex-login` still exists and still works, but it is a terminal command for
/// a GUI application: a user who double-clicks the exe has no way to reach it, and
/// telling them to open a shell to sign in is telling them not to bother.
pub fn sign_in(tx: Sender<AccountEvent>) {
    std::thread::spawn(move || {
        let _ = tx.send(sign_in_blocking());
    });
}

#[cfg(feature = "yandex")]
fn verify_blocking(token: &str) -> AccountEvent {
    use dancer_yandex::{Yandex, YandexError};

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => return AccountEvent::Unknown(e.to_string()),
    };
    // The scratch path is never used by `verify`; the client just needs one.
    let yandex = match Yandex::new(token, std::env::temp_dir()) {
        Ok(y) => y,
        Err(e) => return AccountEvent::Unknown(e.to_string()),
    };

    match runtime.block_on(yandex.verify()) {
        Ok(account) if !account.service_available => {
            // A valid token on an account without Yandex Music is a real state, and
            // it otherwise fails much later, at the download, where the cause is far
            // less obvious.
            AccountEvent::Unknown(format!(
                "{} is signed in, but Yandex Music is not available for that account",
                account.display()
            ))
        }
        Ok(account) => AccountEvent::Valid { login: account.display() },
        Err(YandexError::TokenRejected) => AccountEvent::Rejected,
        Err(e) => AccountEvent::Unknown(e.to_string()),
    }
}

#[cfg(feature = "yandex")]
fn sign_in_blocking() -> AccountEvent {
    use std::sync::mpsc;

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => return AccountEvent::SignInFailed(e.to_string()),
    };
    let device = crate::hostname().unwrap_or_else(|| "dancer-rs".into());

    // The code arrives from inside the flow, which then keeps polling. Showing a
    // modal in that callback would block the polling it is waiting on, so the code
    // is handed to a separate thread to display.
    let (code_tx, code_rx) = mpsc::channel::<dancer_yandex::DeviceLogin>();
    let shown = std::thread::spawn(move || {
        let Ok(code) = code_rx.recv() else {
            // The flow failed before producing a code; there is nothing to show and
            // the error is reported by the main path.
            return;
        };
        dialog::open_url(&code.verification_url);
        let minutes = code.expires_in.map(|d| d.as_secs() / 60).unwrap_or(5);
        let t = crate::i18n::t();
        dialog::info(t.sign_in_code_title, &(t.sign_in_code_body)(&code.user_code, minutes));
    });

    let result = runtime.block_on(dancer_yandex::login(&device, |code| {
        // Send and return immediately. Anything slow here stalls the poll loop.
        let _ = code_tx.send(code.clone());
    }));
    // Whether or not the dialog was dismissed, the flow is over.
    drop(shown);

    match result {
        Ok(token) => {
            // Name the account straight away, so the confirmation can say who was
            // signed in rather than just that something worked.
            let login = match verify_blocking(&token) {
                AccountEvent::Valid { login } => login,
                _ => "signed in".to_string(),
            };
            AccountEvent::SignedIn { token, login }
        }
        Err(e) => AccountEvent::SignInFailed(e.to_string()),
    }
}

#[cfg(not(feature = "yandex"))]
fn verify_blocking(_token: &str) -> AccountEvent {
    AccountEvent::Unknown("this build has the `yandex` feature disabled".into())
}

#[cfg(not(feature = "yandex"))]
fn sign_in_blocking() -> AccountEvent {
    AccountEvent::SignInFailed("this build has the `yandex` feature disabled".into())
}

/// One-line summary for the tray menu.
pub fn status_line(state: &Status) -> String {
    let t = crate::i18n::t();
    match state {
        Status::Off => t.yandex_off.into(),
        Status::Checking => t.yandex_checking.into(),
        Status::Ok(login) => (t.yandex_as)(login),
        Status::Rejected => t.yandex_expired.into(),
        Status::Unavailable => t.yandex_unreachable.into(),
    }
}

/// What the app believes about the stored token.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// No token stored. Not an error — the feature is opt-in.
    Off,
    Checking,
    Ok(String),
    /// The token was rejected. This is the one state worth interrupting for.
    Rejected,
    /// Could not check. The token may be perfectly good.
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_token_is_not_a_failure() {
        // Prompting someone who never asked for the feature is nagging. `verify`
        // returns without spawning anything, so nothing is ever reported.
        let ch = Channel::new();
        verify(String::new(), ch.tx.clone());
        verify("   ".into(), ch.tx.clone());
        assert!(ch.rx.try_recv().is_err(), "an absent token reported something");
    }

    #[test]
    fn every_status_says_something_specific() {
        // The tray line is the only place most users will ever learn about this, so
        // "error" is not an acceptable rendering of any of these.
        let lines = [
            status_line(&Status::Off),
            status_line(&Status::Checking),
            status_line(&Status::Ok("someone".into())),
            status_line(&Status::Rejected),
            status_line(&Status::Unavailable),
        ];
        for l in &lines {
            assert!(l.starts_with("Yandex"), "{l}");
        }
        // Distinguishable from each other, or the state machine is pointless.
        for (i, a) in lines.iter().enumerate() {
            for b in &lines[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_rejected_token_is_distinct_from_an_unreachable_api() {
        // Only one of these means "sign in again". Collapsing them would prompt
        // people to re-authenticate every time their network hiccups.
        assert_ne!(status_line(&Status::Rejected), status_line(&Status::Unavailable));
    }
}
