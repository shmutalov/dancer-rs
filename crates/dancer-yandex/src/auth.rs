//! Getting a token without asking the user to extract one (spec §6.4).
//!
//! The first implementation expected a token pasted into `config.toml`, which
//! meant telling the user to dig one out of the desktop client's storage or a
//! browser session. That is a bad instruction on three counts: it is fiddly, it
//! teaches people to go hunting for credentials in application internals, and what
//! they find is indistinguishable from what a credential stealer would want.
//!
//! Yandex's OAuth **device flow** does the same job properly. The app shows a short
//! code, the user types it into a Yandex page in their own browser, and Yandex
//! hands the app a token. The user authenticates with Yandex — never with us — and
//! can revoke it from their account page afterwards.

use std::time::Duration;

use yamuse::client::Client;
use yamuse::DeviceAuthOptions;

use crate::YandexError;

/// What to show the user while the flow is waiting.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceLogin {
    /// Short code the user types into the Yandex page.
    pub user_code: String,
    /// Page to open.
    pub verification_url: String,
    /// How long the code stays valid.
    pub expires_in: Option<Duration>,
}

/// Run the device flow and return an access token.
///
/// `show` is called once, as soon as the code exists, and must display it — the
/// call then blocks until the user confirms or the code expires, so a caller that
/// prints afterwards would show the code after it was needed.
pub async fn login(
    device_name: &str,
    show: impl FnOnce(&DeviceLogin),
) -> Result<String, YandexError> {
    // Anonymous: there is no token yet, which is the entire point.
    let client = Client::anonymous().map_err(|e| YandexError::Api(e.to_string()))?;

    let options = DeviceAuthOptions {
        device_name: device_name.to_string(),
        ..Default::default()
    };

    let token = client
        .device_auth(options, |code| {
            show(&DeviceLogin {
                user_code: code.user_code.clone().unwrap_or_default(),
                verification_url: code
                    .verification_url
                    .clone()
                    // The flow is useless without somewhere to send the user, and
                    // this is the documented page when the response omits one.
                    .unwrap_or_else(|| "https://oauth.yandex.ru/device".into()),
                expires_in: code.expires_in.map(Duration::from_secs),
            });
        })
        .await
        .map_err(|e| YandexError::Api(e.to_string()))?;

    token
        .access_token
        .filter(|t| !t.trim().is_empty())
        // A grant that carries no token is a failure, not an empty success — and
        // storing an empty string would silently disable the feature later.
        .ok_or_else(|| YandexError::Api("device flow returned no access token".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_login_carries_what_the_user_needs_to_see() {
        let d = DeviceLogin {
            user_code: "ABCD-1234".into(),
            verification_url: "https://oauth.yandex.ru/device".into(),
            expires_in: Some(Duration::from_secs(300)),
        };
        assert!(!d.user_code.is_empty());
        assert!(d.verification_url.starts_with("https://"));
    }
}
